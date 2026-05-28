/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <gtest/gtest.h>
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
