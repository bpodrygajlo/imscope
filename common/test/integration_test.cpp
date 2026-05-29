/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <gtest/gtest.h>
#include <nng/nng.h>
#include <nng/protocol/reqrep0/req.h>
#include <spdlog/spdlog.h>
#include <chrono>
#include <thread>
#include "imscope_consumer.h"
#include "imscope_producer.h"

TEST(IntegrationTest, ProducerConsumerInprocMultiScope) {
  spdlog::set_level(spdlog::level::debug);

  const char* data_addr = "inproc://data_multi";
  const char* announce_addr = "inproc://announce_multi";
  const char* producer_name = "test_producer_multi";

  imscope_scope_desc_t scopes[] = {{"scope1", SCOPE_TYPE_REAL},
                                   {"scope2", SCOPE_TYPE_REAL}};

  // Initialize Producer
  imscope_return_t res =
      imscope_init_producer(data_addr, announce_addr, producer_name, scopes, 2);
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // Give some time for threads to start
  std::this_thread::sleep_for(std::chrono::milliseconds(100));

  // Initialize Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
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
  // Use a small retry loop because of internal NNG latency
  int retry = 0;
  while ((res = imscope_try_send_data(data1, 0, num_samples, 1, 0,
                                      timestamp)) == IMSCOPE_ERROR_BUSY &&
         retry < 100) {
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
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
  const char* data_addr = "inproc://data_zerocopy";
  const char* announce_addr = "inproc://announce_zerocopy";

  imscope_scope_desc_t scopes[] = {{"zerocopy_scope", SCOPE_TYPE_REAL}};

  imscope_return_t rv = imscope_init_producer(data_addr, announce_addr,
                                              "ZeroCopyProducer", scopes, 1);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
  ASSERT_NE(consumer, nullptr);

  // 1. Request data (synchronous REQ/REP)
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
  const char* data_addr = "inproc://data_dynamic_send";
  const char* announce_addr = "inproc://announce_dynamic_send";

  // Initialize Producer with NO scopes
  imscope_return_t rv = imscope_init_producer(
      data_addr, announce_addr, "DynamicSendProducer", nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Connect Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
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
  const char* data_addr = "inproc://data_dynamic_zc";
  const char* announce_addr = "inproc://announce_dynamic_zc";

  // Initialize Producer with NO scopes
  imscope_return_t rv = imscope_init_producer(data_addr, announce_addr,
                                              "DynamicZCProducer", nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Connect Consumer
  ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
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
  const char* data_addr = "inproc://data_scalar";
  const char* announce_addr = "inproc://announce_scalar";

  imscope_scope_desc_t scopes[] = {{"scope_int32", SCOPE_TYPE_INT32},
                                   {"scope_float", SCOPE_TYPE_FLOAT}};

  imscope_return_t rv = imscope_init_producer(data_addr, announce_addr,
                                              "ScalarProducer", scopes, 2);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
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
  const char* data_addr = "inproc://data_settings";
  const char* announce_addr = "inproc://announce_settings";
  const char* control_addr = "inproc://announce_settings-control";

  imscope_return_t rv = imscope_init_producer(data_addr, announce_addr,
                                              "SettingsProducer", nullptr, 0);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Register settings after initialization
  imscope_register_setting_bool("bool_setting", true, test_bool_cb);
  imscope_register_setting_int32("int_setting", 42, test_int32_cb);
  imscope_register_setting_float("float_setting", 3.14f, test_float_cb);

  // Connect control client
  nng_socket ctrl_sock;
  nng_req0_open(&ctrl_sock);
  int nng_res = nng_dial(ctrl_sock, control_addr, NULL, 0);
  ASSERT_EQ(nng_res, 0);

  // 1. Send GET_ALL request
  setting_request_t req = {};
  req.magic = SETTING_REQ_GET_ALL;
  nng_msg* req_msg;
  nng_msg_alloc(&req_msg, sizeof(setting_request_t));
  memcpy(nng_msg_body(req_msg), &req, sizeof(setting_request_t));

  nng_res = nng_sendmsg(ctrl_sock, req_msg, 0);
  ASSERT_EQ(nng_res, 0);

  nng_msg* rep_msg;
  nng_res = nng_recvmsg(ctrl_sock, &rep_msg, 0);
  ASSERT_EQ(nng_res, 0);

  setting_response_t* rep = (setting_response_t*)nng_msg_body(rep_msg);
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
  nng_msg_free(rep_msg);

  // 2. Send SET request for bool_setting
  setting_request_t set_req = {};
  set_req.magic = SETTING_REQ_SET;
  strcpy(set_req.name, "bool_setting");
  set_req.type = SETTING_TYPE_BOOL;
  set_req.value.bval = 0;  // false

  nng_msg_alloc(&req_msg, sizeof(setting_request_t));
  memcpy(nng_msg_body(req_msg), &set_req, sizeof(setting_request_t));
  nng_res = nng_sendmsg(ctrl_sock, req_msg, 0);
  ASSERT_EQ(nng_res, 0);

  nng_res = nng_recvmsg(ctrl_sock, &rep_msg, 0);
  ASSERT_EQ(nng_res, 0);

  rep = (setting_response_t*)nng_msg_body(rep_msg);
  EXPECT_EQ(rep->magic, SETTING_REP_SET);
  EXPECT_EQ(rep->status, 0);
  nng_msg_free(rep_msg);

  // Check callback
  EXPECT_TRUE(bool_cb_called);
  EXPECT_FALSE(bool_cb_val);

  // 3. Send SET request for int_setting
  memset(&set_req, 0, sizeof(set_req));
  set_req.magic = SETTING_REQ_SET;
  strcpy(set_req.name, "int_setting");
  set_req.type = SETTING_TYPE_INT32;
  set_req.value.ival = 100;

  nng_msg_alloc(&req_msg, sizeof(setting_request_t));
  memcpy(nng_msg_body(req_msg), &set_req, sizeof(setting_request_t));
  nng_res = nng_sendmsg(ctrl_sock, req_msg, 0);
  ASSERT_EQ(nng_res, 0);

  nng_res = nng_recvmsg(ctrl_sock, &rep_msg, 0);
  ASSERT_EQ(nng_res, 0);

  rep = (setting_response_t*)nng_msg_body(rep_msg);
  EXPECT_EQ(rep->magic, SETTING_REP_SET);
  EXPECT_EQ(rep->status, 0);
  nng_msg_free(rep_msg);

  // Check callback
  EXPECT_TRUE(int32_cb_called);
  EXPECT_EQ(int32_cb_val, 100);

  nng_close(ctrl_sock);
  imscope_cleanup_producer();
}
