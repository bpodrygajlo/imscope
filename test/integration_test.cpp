#include <gtest/gtest.h>
#include <spdlog/spdlog.h>
#include "imscope_producer.h"
#include "imscope_consumer.h"
#include <thread>
#include <chrono>

TEST(IntegrationTest, ProducerConsumerInproc) {
    spdlog::set_level(spdlog::level::debug);

    const char* control_addr = "inproc://control";
    const char* data_addr = "inproc://data";
    const char* announce_addr = "inproc://announce";
    const char* producer_name = "test_producer";

    imscope_scope_desc_t scopes[] = {
        {"scope1", SCOPE_TYPE_REAL}
    };

    // Initialize Producer
    // Note: imscope_init_producer uses a singleton, so this can only be called once per process effectively
    int res = imscope_init_producer(control_addr, data_addr, announce_addr, producer_name, scopes, 1);
    ASSERT_EQ(res, 0);

    // Give some time for threads to start
    std::this_thread::sleep_for(std::chrono::milliseconds(100));

    // Initialize Consumer
    ImscopeConsumer* consumer = ImscopeConsumer::connect(announce_addr);
    ASSERT_NE(consumer, nullptr);
    ASSERT_EQ(consumer->get_name(), producer_name);
    ASSERT_EQ(consumer->get_num_scopes(), 1);
    ASSERT_STREQ(consumer->get_scope_name(0), "scope1");

    // Request credits (flow control)
    consumer->request_scope_data(0, 10);

    // Allow credit request to propagate
    std::this_thread::sleep_for(std::chrono::milliseconds(100));

    // Send data
    uint32_t data[] = {1, 2, 3, 4, 5};
    size_t num_samples = 5;
    uint64_t timestamp = 1000;

    bool received = false;
    for (int i = 0; i < 20; ++i) {
        imscope_send_data(data, 0, num_samples, 1, 0, timestamp);

        int version = -1;
        // try_collect_scope_msg returns a shared_ptr<void> with a custom deleter
        auto msg_ptr = consumer->try_collect_scope_msg(0, version);
        if (msg_ptr) {
            scope_msg_t* msg = (scope_msg_t*)msg_ptr.get();
            EXPECT_EQ(msg->id, 0);
            EXPECT_EQ(msg->meta.frame, 1);
            EXPECT_EQ(msg->meta.timestamp, timestamp);
            EXPECT_EQ(msg->data_size, num_samples * sizeof(uint32_t));

            // data follows the struct
            uint32_t* received_data = (uint32_t*)(msg + 1);
            for (size_t j = 0; j < num_samples; ++j) {
                EXPECT_EQ(received_data[j], data[j]);
            }
            received = true;
            break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
    }

    ASSERT_TRUE(received) << "Did not receive data from producer";

    delete consumer;
}
