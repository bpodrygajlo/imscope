/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <gtest/gtest.h>
#include <spdlog/spdlog.h>
#include <zenoh.hxx>
#include <atomic>
#include <chrono>
#include <cstring>
#include <string>
#include <thread>
#include <variant>
#include <vector>
#include "imscope_common.h"
#include "imscope_consumer.h"
#include "imscope_internal.h"
#include "imscope_producer.h"

// ── Deterministic, same-machine Zenoh pairing for tests ─────────────────────
//
// Zenoh has no direct equivalent to NNG's named `inproc://` transport, so
// this file's producer/consumer pairs are wired together over an explicit
// `tcp/127.0.0.1:<port>` endpoint instead: the producer session is made to
// also *listen* on that endpoint (via the `g_test_extra_listen_locator` test
// hook in `imscope_internal.h`) and the consumer session *connects* to it
// directly. This avoids depending on Zenoh's default multicast scouting,
// which may be slow or unavailable in sandboxed/CI environments. Multicast
// scouting is also disabled outright (`g_test_disable_multicast_scouting`)
// for every session opened while running this binary, for determinism.
//
// Each test gets its own port (a simple incrementing counter) so that
// back-to-back tests in the same process never race to bind the same port
// while a previous test's socket is still being released.

namespace {

std::atomic<int> g_next_test_port{27500};

std::string next_test_endpoint() {
  int port = g_next_test_port.fetch_add(1);
  return "tcp/127.0.0.1:" + std::to_string(port);
}

// Runs once, before any TEST() body in this binary, so every session opened
// by this test binary -- including ones opened deep inside
// `imscope_init_producer`/`ImscopeConsumer::connect` -- skips multicast
// scouting.
bool g_disable_scouting_for_tests = [] {
  g_test_disable_multicast_scouting = true;
  return true;
}();

// Initializes a producer that also listens on `endpoint`, in addition to
// whatever `imscope_init_producer` itself does. Mirrors how a real caller
// would invoke `imscope_init_producer`, plus the test-only listen hook.
imscope_return_t init_test_producer(const std::string& endpoint,
                                    const char* name,
                                    imscope_scope_desc_t* scopes,
                                    size_t num_scopes) {
  g_test_extra_listen_locator = endpoint;
  imscope_return_t res = imscope_init_producer("", "", name, scopes, num_scopes);
  g_test_extra_listen_locator.clear();
  return res;
}

// Sends one raw control request/reply round trip directly against a
// producer's control key expression, bypassing `ImscopeConsumer` -- used by
// the settings test below, which wants to exercise the wire-level control
// protocol directly the way the old test dialed the NNG control socket
// directly.
std::vector<uint8_t> control_request(zenoh::Session& session, const std::string& name,
                                     const void* req, size_t req_size) {
  std::string key = "imscope/" + name + "/control";
  std::vector<uint8_t> req_bytes(req_size);
  std::memcpy(req_bytes.data(), req, req_size);

  zenoh::Session::GetOptions options = zenoh::Session::GetOptions::create_default();
  options.payload = zenoh::Bytes(std::move(req_bytes));
  options.timeout_ms = 2000;

  auto replies = session.get(key, "", zenoh::channels::FifoChannel(1), std::move(options));
  auto res = replies.recv();
  if (!std::holds_alternative<zenoh::Reply>(res)) {
    return {};
  }
  zenoh::Reply& reply = std::get<zenoh::Reply>(res);
  if (!reply.is_ok()) {
    return {};
  }
  return reply.get_ok().get_payload().as_vector();
}

}  // namespace

TEST(IntegrationTest, ProducerConsumerInprocMultiScope) {
  spdlog::set_level(spdlog::level::debug);

  std::string endpoint = next_test_endpoint();
  const char* producer_name = "test_producer_multi";

  imscope_scope_desc_t scopes[] = {{"scope1", SCOPE_TYPE_REAL},
                                   {"scope2", SCOPE_TYPE_REAL}};

  // Initialize Producer
  imscope_return_t res = init_test_producer(endpoint, producer_name, scopes, 2);
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // Give some time for the session/queryables to be ready. Opening the
  // very first Zenoh session in the process can carry some one-time
  // runtime warm-up cost, so this is a bit more generous than the
  // steady-state latency later sends see.
  std::this_thread::sleep_for(std::chrono::milliseconds(200));

  // Initialize Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect(producer_name, endpoint.c_str());
  ASSERT_NE(consumer, nullptr);
  ASSERT_EQ(consumer->get_name(), producer_name);
  ASSERT_EQ(consumer->get_num_scopes(), 2);

  // Trigger initial requests
  consumer->request_data(0);
  consumer->request_data(1);

  // Data to send
  uint32_t data1[] = {1, 2, 3};
  uint32_t data2[] = {10, 20, 30};
  size_t num_samples = 3;
  uint64_t timestamp = 1000;

  // 1. Send data for scope 0 (succeeds)
  // Use a retry loop because of internal Zenoh latency -- this is the
  // first request/reply round trip of the test, which can occasionally be
  // slower than later ones (session/transport establishment isn't
  // necessarily fully settled the instant `connect()` returns), so this
  // loop gets a more generous budget than later ones in this file.
  int retry = 0;
  while ((res = imscope_try_send_data(data1, 0, num_samples, 1, 0,
                                      timestamp)) == IMSCOPE_ERROR_BUSY &&
         retry < 500) {
    std::this_thread::sleep_for(std::chrono::milliseconds(2));
    retry++;
  }
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // 2. Verify scope 0 is busy (no request sent yet for second message)
  res = imscope_try_send_data(data1, 0, num_samples, 1, 0, timestamp);
  ASSERT_EQ(res, IMSCOPE_ERROR_BUSY);  // Busy

  // 3. Send data for scope 1
  res = imscope_try_send_data(data2, 1, num_samples, 1, 0, timestamp);
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // 4. Verify scope 1 is busy
  res = imscope_try_send_data(data2, 1, num_samples, 1, 0, timestamp);
  ASSERT_EQ(res, IMSCOPE_ERROR_BUSY);  // Busy

  // 5. Collect both messages and hold them
  int v0 = -1, v1 = -1;
  NngMsgPtr msg0, msg1;

  for (int i = 0; i < 40; ++i) {
    if (!msg0)
      msg0 = consumer->try_collect_scope_msg(0, v0);
    if (!msg1)
      msg1 = consumer->try_collect_scope_msg(1, v1);
    if (msg0 && msg1)
      break;
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
  }

  ASSERT_TRUE(msg0) << "Did not receive data for scope 0";
  ASSERT_TRUE(msg1) << "Did not receive data for scope 1";

  // 6. Verify data while holding both (both have NOT sent REP yet)
  scope_msg_t* m0 = (scope_msg_t*)msg0.get();
  scope_msg_t* m1 = (scope_msg_t*)msg1.get();
  EXPECT_EQ(m0->id, 0);
  EXPECT_EQ(m1->id, 1);

  // Verify producer is STILL busy because REP hasn't been sent
  res = imscope_try_send_data(data1, 0, num_samples, 1, 0, timestamp);
  EXPECT_EQ(res, IMSCOPE_ERROR_BUSY);
  res = imscope_try_send_data(data2, 1, num_samples, 1, 0, timestamp);
  EXPECT_EQ(res, IMSCOPE_ERROR_BUSY);

  // 7. Release messages and request more
  msg0.reset();
  msg1.reset();

  consumer->request_data(0);
  consumer->request_data(1);

  // Give some time for REPs to be processed
  std::this_thread::sleep_for(std::chrono::milliseconds(200));

  // 8. Verify producer is no longer busy
  res = imscope_try_send_data(data1, 0, num_samples, 2, 0, timestamp + 1);
  EXPECT_EQ(res, IMSCOPE_SUCCESS);
  res = imscope_try_send_data(data2, 1, num_samples, 2, 0, timestamp + 1);
  EXPECT_EQ(res, IMSCOPE_SUCCESS);

  delete consumer;
  imscope_cleanup_producer();
}

TEST(IntegrationTest, ZeroCopySend) {
  std::string endpoint = next_test_endpoint();

  imscope_scope_desc_t scopes[] = {{"zerocopy_scope", SCOPE_TYPE_REAL}};

  imscope_return_t rv =
      init_test_producer(endpoint, "ZeroCopyProducer", scopes, 1);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  ImscopeConsumer* consumer = ImscopeConsumer::connect("ZeroCopyProducer", endpoint.c_str());
  ASSERT_NE(consumer, nullptr);

  // 1. Request data
  consumer->request_data(0);

  // 2. Zero-copy send
  void* buf = nullptr;
  int retry = 0;
  size_t num_samples = 100;
  while ((buf = imscope_acquire_send_buffer(0, num_samples)) == nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(buf, nullptr);

  uint32_t* iq_buf = (uint32_t*)buf;
  for (size_t i = 0; i < num_samples; ++i) {
    iq_buf[i] = i;
  }

  imscope_return_t res =
      imscope_commit_send_buffer(0, num_samples, 10, 5, 2000);
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // Consume
  int handle = 0;
  NngMsgPtr msg_ptr = nullptr;
  retry = 0;
  while ((msg_ptr = consumer->try_collect_scope_msg(0, handle)) == nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(msg_ptr, nullptr) << "Did not receive zero-copy message";

  scope_msg_t* msg = (scope_msg_t*)msg_ptr.get();
  ASSERT_EQ(msg->meta.frame, 10);
  ASSERT_EQ(msg->meta.slot, 5);
  ASSERT_EQ(msg->meta.timestamp, 2000);
  ASSERT_EQ(msg->data_size, num_samples * sizeof(uint32_t));

  uint32_t* recv_data = (uint32_t*)(msg + 1);
  for (size_t i = 0; i < num_samples; ++i) {
    ASSERT_EQ(recv_data[i], i);
  }

  delete consumer;
  imscope_cleanup_producer();
}

TEST(IntegrationTest, DynamicRegistrationBySend) {
  std::string endpoint = next_test_endpoint();

  // Initialize Producer with NO scopes
  imscope_return_t rv =
      init_test_producer(endpoint, "DynamicSendProducer", nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Connect Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect("DynamicSendProducer", endpoint.c_str());
  ASSERT_NE(consumer, nullptr);
  EXPECT_EQ(consumer->get_num_scopes(), 0);  // No scopes initially

  // Send data to a new scope "dyn_scope" (type REAL).
  // This should register it on-the-fly.
  uint32_t data[] = {42, 43, 44};
  size_t num_samples = 3;

  // Since we haven't requested data, it should return BUSY (it only sends when a request is active)
  rv = imscope_try_send_data_by_name(data, "dyn_scope", SCOPE_TYPE_REAL,
                                     num_samples, 10, 5, 100);
  EXPECT_EQ(rv, IMSCOPE_ERROR_BUSY);

  // Now the consumer refreshes scopes
  bool refreshed = consumer->refresh_scopes();
  EXPECT_TRUE(refreshed);
  EXPECT_EQ(consumer->get_num_scopes(), 1);
  EXPECT_STREQ(consumer->get_scope_name(0), "dyn_scope");

  // Consumer requests data for scope 0
  consumer->request_data(0);

  // Wait for the request to be active/received on the producer side
  std::this_thread::sleep_for(std::chrono::milliseconds(50));

  // Now, imscope_try_send_data_by_name should succeed!
  rv = imscope_try_send_data_by_name(data, "dyn_scope", SCOPE_TYPE_REAL,
                                     num_samples, 10, 5, 100);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Let's collect the message and verify
  int handle = 0;
  NngMsgPtr msg_ptr = nullptr;
  int retry = 0;
  while ((msg_ptr = consumer->try_collect_scope_msg(0, handle)) == nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(msg_ptr, nullptr);

  scope_msg_t* msg = (scope_msg_t*)msg_ptr.get();
  EXPECT_EQ(msg->meta.frame, 10);
  EXPECT_EQ(msg->meta.slot, 5);
  EXPECT_EQ(msg->meta.timestamp, 100);
  EXPECT_EQ(msg->data_size, num_samples * sizeof(uint32_t));

  uint32_t* recv_data = (uint32_t*)(msg + 1);
  EXPECT_EQ(recv_data[0], 42);
  EXPECT_EQ(recv_data[1], 43);
  EXPECT_EQ(recv_data[2], 44);

  delete consumer;
  imscope_cleanup_producer();
}

TEST(IntegrationTest, DynamicRegistrationZeroCopyByName) {
  std::string endpoint = next_test_endpoint();

  // Initialize Producer with NO scopes
  imscope_return_t rv =
      init_test_producer(endpoint, "DynamicZCProducer", nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Connect Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect("DynamicZCProducer", endpoint.c_str());
  ASSERT_NE(consumer, nullptr);
  EXPECT_EQ(consumer->get_num_scopes(), 0);

  // Attempt to acquire buffer. It will register the scope but return nullptr since no request is active.
  void* buf =
      imscope_acquire_send_buffer_by_name("dyn_zc", SCOPE_TYPE_REAL, 10);
  EXPECT_EQ(buf, nullptr);

  // Consumer refreshes scopes
  bool refreshed = consumer->refresh_scopes();
  EXPECT_TRUE(refreshed);
  EXPECT_EQ(consumer->get_num_scopes(), 1);
  EXPECT_STREQ(consumer->get_scope_name(0), "dyn_zc");

  // Consumer requests data
  consumer->request_data(0);

  // Wait for the request to be active/received
  std::this_thread::sleep_for(std::chrono::milliseconds(50));

  // Now, acquiring the buffer by name should succeed!
  buf = imscope_acquire_send_buffer_by_name("dyn_zc", SCOPE_TYPE_REAL, 10);
  ASSERT_NE(buf, nullptr);

  uint32_t* iq_buf = (uint32_t*)buf;
  for (int i = 0; i < 10; ++i) {
    iq_buf[i] = i * 10;
  }

  // Commit send buffer by name
  rv = imscope_commit_send_buffer_by_name("dyn_zc", 10, 20, 10, 200);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Collect message and verify
  int handle = 0;
  NngMsgPtr msg_ptr = nullptr;
  int retry = 0;
  while ((msg_ptr = consumer->try_collect_scope_msg(0, handle)) == nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(msg_ptr, nullptr);

  scope_msg_t* msg = (scope_msg_t*)msg_ptr.get();
  EXPECT_EQ(msg->meta.frame, 20);
  EXPECT_EQ(msg->meta.slot, 10);
  EXPECT_EQ(msg->meta.timestamp, 200);
  EXPECT_EQ(msg->data_size, 10 * sizeof(uint32_t));

  uint32_t* recv_data = (uint32_t*)(msg + 1);
  for (int i = 0; i < 10; ++i) {
    EXPECT_EQ(recv_data[i], i * 10);
  }

  delete consumer;
  imscope_cleanup_producer();
}

TEST(IntegrationTest, ScalarPublishing) {
  std::string endpoint = next_test_endpoint();

  imscope_scope_desc_t scopes[] = {{"scope_int32", SCOPE_TYPE_INT32},
                                   {"scope_float", SCOPE_TYPE_FLOAT}};

  imscope_return_t rv = init_test_producer(endpoint, "ScalarProducer", scopes, 2);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  ImscopeConsumer* consumer = ImscopeConsumer::connect("ScalarProducer", endpoint.c_str());
  ASSERT_NE(consumer, nullptr);

  // Trigger requests
  consumer->request_data(0);
  consumer->request_data(1);

  // Sleep slightly to let consumer request reach the producer
  std::this_thread::sleep_for(std::chrono::milliseconds(50));

  // Send int32 values.
  for (int32_t i = 0; i < 10; ++i) {
    imscope_try_send_int32(i * 100, 0);
  }

  // Send float values by name
  for (int i = 0; i < 10; ++i) {
    imscope_try_send_float_by_name(i * 1.5f, "scope_float");
  }

  // Since we only sent 10, the accumulator will not flush immediately by threshold (256).
  // We wait 60ms (timeout is 30ms) so it auto-flushes.
  std::this_thread::sleep_for(std::chrono::milliseconds(60));

  for (int32_t i = 0; i < 10; ++i) {
    imscope_try_send_int32(i * 100, 0);
  }

  // Send float values by name
  for (int i = 0; i < 10; ++i) {
    imscope_try_send_float_by_name(i * 1.5f, "scope_float");
  }

  // Now consume
  int handle_int = 0;
  NngMsgPtr msg_int = nullptr;
  int retry = 0;
  while ((msg_int = consumer->try_collect_scope_msg(0, handle_int)) ==
             nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(msg_int, nullptr);

  scope_msg_t* m_int = (scope_msg_t*)msg_int.get();
  EXPECT_EQ(m_int->data_size, 10 * sizeof(int32_t));
  int32_t* data_int = (int32_t*)(m_int + 1);
  for (int i = 0; i < 10; ++i) {
    EXPECT_EQ(data_int[i], i * 100);
  }

  int handle_float = 0;
  NngMsgPtr msg_float = nullptr;
  retry = 0;
  while ((msg_float = consumer->try_collect_scope_msg(1, handle_float)) ==
             nullptr &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
    retry++;
  }
  ASSERT_NE(msg_float, nullptr);

  scope_msg_t* m_float = (scope_msg_t*)msg_float.get();
  EXPECT_EQ(m_float->data_size, 10 * sizeof(float));
  float* data_float = (float*)(m_float + 1);
  for (int i = 0; i < 10; ++i) {
    EXPECT_NEAR(data_float[i], i * 1.5f, 1e-5f);
  }

  delete consumer;
  imscope_cleanup_producer();
}

static bool bool_cb_called = false;
static bool bool_cb_val = false;

void test_bool_cb(bool val) {
  bool_cb_called = true;
  bool_cb_val = val;
}

static bool int32_cb_called = false;
static int32_t int32_cb_val = 0;

void test_int32_cb(int32_t val) {
  int32_cb_called = true;
  int32_cb_val = val;
}

static bool float_cb_called = false;
static float float_cb_val = 0.0f;

void test_float_cb(float val) {
  float_cb_called = true;
  float_cb_val = val;
}

TEST(IntegrationTest, SettingsManagement) {
  std::string endpoint = next_test_endpoint();
  const char* producer_name = "SettingsProducer";

  imscope_return_t rv = init_test_producer(endpoint, producer_name, nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Register settings after initialization
  imscope_register_setting_bool("bool_setting", true, test_bool_cb);
  imscope_register_setting_int32("int_setting", 42, test_int32_cb);
  imscope_register_setting_float("float_setting", 3.14f, test_float_cb);

  // Connect a raw control client directly against the control key
  // expression (mirrors the old test dialing the NNG control socket
  // directly, to exercise the wire-level settings protocol without going
  // through ImscopeConsumer).
  zenoh::Session ctrl_session = open_zenoh_session(endpoint.c_str());

  // 1. Send GET_ALL request
  setting_request_t req = {};
  req.magic = SETTING_REQ_GET_ALL;
  std::vector<uint8_t> rep_bytes =
      control_request(ctrl_session, producer_name, &req, sizeof(req));
  ASSERT_GE(rep_bytes.size(), sizeof(setting_response_t) - sizeof(imscope_setting_t));

  setting_response_t* rep = (setting_response_t*)rep_bytes.data();
  EXPECT_EQ(rep->magic, SETTING_REP_GET_ALL);
  EXPECT_EQ(rep->status, 0);
  EXPECT_EQ(rep->num_settings, 3);

  // Check details
  bool found_bool = false, found_int = false, found_float = false;
  for (int i = 0; i < rep->num_settings; ++i) {
    if (strcmp(rep->settings[i].name, "bool_setting") == 0) {
      found_bool = true;
      EXPECT_EQ(rep->settings[i].type, SETTING_TYPE_BOOL);
      EXPECT_EQ(rep->settings[i].value.bval, 1);
    } else if (strcmp(rep->settings[i].name, "int_setting") == 0) {
      found_int = true;
      EXPECT_EQ(rep->settings[i].type, SETTING_TYPE_INT32);
      EXPECT_EQ(rep->settings[i].value.ival, 42);
    } else if (strcmp(rep->settings[i].name, "float_setting") == 0) {
      found_float = true;
      EXPECT_EQ(rep->settings[i].type, SETTING_TYPE_FLOAT);
      EXPECT_NEAR(rep->settings[i].value.fval, 3.14f, 1e-5f);
    }
  }
  EXPECT_TRUE(found_bool);
  EXPECT_TRUE(found_int);
  EXPECT_TRUE(found_float);

  // 2. Send SET request for bool_setting
  setting_request_t set_req = {};
  set_req.magic = SETTING_REQ_SET;
  strcpy(set_req.name, "bool_setting");
  set_req.type = SETTING_TYPE_BOOL;
  set_req.value.bval = 0;  // false

  rep_bytes = control_request(ctrl_session, producer_name, &set_req, sizeof(set_req));
  ASSERT_GE(rep_bytes.size(), sizeof(setting_response_t) - sizeof(imscope_setting_t));
  rep = (setting_response_t*)rep_bytes.data();
  EXPECT_EQ(rep->magic, SETTING_REP_SET);
  EXPECT_EQ(rep->status, 0);

  // Check callback
  EXPECT_TRUE(bool_cb_called);
  EXPECT_FALSE(bool_cb_val);

  // 3. Send SET request for int_setting
  memset(&set_req, 0, sizeof(set_req));
  set_req.magic = SETTING_REQ_SET;
  strcpy(set_req.name, "int_setting");
  set_req.type = SETTING_TYPE_INT32;
  set_req.value.ival = 100;

  rep_bytes = control_request(ctrl_session, producer_name, &set_req, sizeof(set_req));
  ASSERT_GE(rep_bytes.size(), sizeof(setting_response_t) - sizeof(imscope_setting_t));
  rep = (setting_response_t*)rep_bytes.data();
  EXPECT_EQ(rep->magic, SETTING_REP_SET);
  EXPECT_EQ(rep->status, 0);

  // Check callback
  EXPECT_TRUE(int32_cb_called);
  EXPECT_EQ(int32_cb_val, 100);

  imscope_cleanup_producer();
}
