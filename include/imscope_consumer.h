#pragma once

#include <nanomsg/nn.h>
#include <cstddef>
#include <deque>
#include <fstream>
#include <mutex>
#include <vector>
#include "imscope_common.h"
#include <memory>
#include <spdlog/spdlog.h>

using NnMsgPtr = std::shared_ptr<void>;

inline NnMsgPtr make_nn_msg_ptr(void* msg) {
    return NnMsgPtr(msg, [](void* p) { nn_freemsg(p); });
}

class SafeQueue {
 private:
  std::deque<NnMsgPtr> queue;
  std::mutex mutex;
  size_t max_size;
  int version_counter = 0;

 public:
  void push(void* msg) {
    std::unique_lock<std::mutex> lock(mutex);
    queue.push_front(make_nn_msg_ptr(msg));
    if (queue.size() > max_size) {
      queue.pop_back();
    }
    version_counter++;
  }

  NnMsgPtr front(int& version) {
    std::unique_lock<std::mutex> lock(mutex);
    if (version != version_counter) {
      version = version_counter;
      return queue.front();
    }
    return nullptr;
  }

  SafeQueue() = default;
  SafeQueue(size_t size) : max_size(size){};
  SafeQueue(SafeQueue&& other) {
    std::unique_lock<std::mutex> lock(other.mutex);
    queue = std::move(other.queue);
    max_size = other.max_size;
    version_counter = other.version_counter;
  }
  SafeQueue& operator=(SafeQueue&& other) {
    if (this != &other) {
      std::unique_lock<std::mutex> lock_this(mutex, std::defer_lock);
      std::unique_lock<std::mutex> lock_other(other.mutex, std::defer_lock);
      std::lock(lock_this, lock_other);
      queue = std::move(other.queue);
      max_size = other.max_size;
      version_counter = other.version_counter;
    }
    return *this;
  }
};

using SafePtrQueue = SafeQueue;

class ImscopeConsumer {
  std::string control_address;
  std::string data_address;
  std::string announce_address;
  int control_socket;
  std::vector<SafePtrQueue> scope_msg_queues;
  std::vector<imscope_scope_config_t> configured_scopes;
  void start_consumer_thread();
  std::string name;

 public:
  ImscopeConsumer(const char* data_address, const char* announce_address,
                  int num_scopes, imscope_scope_config_t* scopes,
                  const char* name);
  NnMsgPtr try_collect_scope_msg(int scope_id, int &handle);
  bool try_collect_iq(int scope_id, std::vector<int16_t>& real,
                      std::vector<int16_t>& imag);
  bool try_collect_real(int scope_id, std::vector<int16_t>& real);
  static ImscopeConsumer* connect(const char* announce_address);

  const std::string& get_name() const { return name; }

  const char* get_scope_name(int scope_id) const {
    return configured_scopes[scope_id].name;
  }

  int get_num_scopes() const { return configured_scopes.size(); }

  void request_scope_data(int scope_id, int credits);
  static void free(scope_msg_t* msg);
};