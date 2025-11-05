#pragma once

#include <fstream>
#include <mutex>
#include <queue>
#include <vector>
#include "imscope_common.h"

template <typename T>
class SafeQueue {
 private:
  std::queue<T> queue;
  std::mutex mutex;

 public:
  void push(T item) {
    std::unique_lock<std::mutex> lock(mutex);

    queue.push(item);
  }

  bool pop(T* item) {
    std::unique_lock<std::mutex> lock(mutex);

    if (queue.empty()) {
      return false;
    }
    *item = queue.front();
    queue.pop();

    return true;
  }

  bool try_front(T* item) {
    std::unique_lock<std::mutex> lock(mutex);
    if (queue.empty()) {
      return false;
    }
    *item = queue.front();
    return true;
  }

  bool try_pop(T* item) {
    if (mutex.try_lock()) {
      if (queue.empty()) {
        mutex.unlock();
        return false;
      }
      *item = queue.front();
      queue.pop();
      mutex.unlock();
      return true;
    }
    return false;
  }

  size_t size() {
    std::unique_lock<std::mutex> lock(mutex);
    return queue.size();
  }

  void empty(int(free_func)(void*)) {
    std::unique_lock<std::mutex> lock(mutex);
    while (!queue.empty()) {
      T item = queue.front();
      queue.pop();
      free_func(item);
    }
  }
};

using SafePtrQueue = SafeQueue<scope_msg_t*>;

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
  scope_msg_t* try_collect_scope_msg(int scope_id);
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

  scope_type_t get_msg_type(int scope_id) const {
    return configured_scopes[scope_id].type;
  }
};