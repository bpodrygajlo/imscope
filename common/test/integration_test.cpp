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

  // Data to send
  uint32_t data1[] = {1, 2, 3};
  uint32_t data2[] = {10, 20, 30};
  size_t num_samples = 3;
  uint64_t timestamp = 1000;

  // 1. Send data for scope 0
  res = imscope_try_send_data(data1, 0, num_samples, 1, 0, timestamp);
  ASSERT_EQ(res, IMSCOPE_SUCCESS);

  // 2. Verify scope 0 is busy
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

  // 7. Release messages (this should trigger REP)
  msg0.reset();
  msg1.reset();

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

  // Wait for discovery
  std::this_thread::sleep_for(std::chrono::milliseconds(200));

  // Acquire buffer
  size_t num_samples = 100;
  void* buffer = imscope_acquire_send_buffer(0, num_samples);
  ASSERT_NE(buffer, nullptr);

  // Fill buffer
  uint32_t* data = static_cast<uint32_t*>(buffer);
  for (size_t i = 0; i < num_samples; ++i)
    data[i] = i;

  // Commit
  rv = imscope_commit_send_buffer(0, num_samples, 123, 4, 1000);
  ASSERT_EQ(rv, IMSCOPE_SUCCESS);

  // Consume
  int handle = 0;
  NngMsgPtr msg_ptr;
  for (int i = 0; i < 40; ++i) {
    msg_ptr = consumer->try_collect_scope_msg(0, handle);
    if (msg_ptr)
      break;
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
  }
  ASSERT_NE(msg_ptr, nullptr) << "Did not receive zero-copy message";

  scope_msg_t* msg = static_cast<scope_msg_t*>(msg_ptr.get());
  ASSERT_EQ(msg->meta.frame, 123);
  ASSERT_EQ(msg->meta.slot, 4);
  ASSERT_EQ(msg->data_size, num_samples * sizeof(uint32_t));

  uint32_t* recv_data = (uint32_t*)(msg + 1);
  for (size_t i = 0; i < num_samples; ++i) {
    ASSERT_EQ(recv_data[i], i);
  }

  delete consumer;
  imscope_cleanup_producer();
}
