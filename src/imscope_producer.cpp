#include "imscope_producer.h"
#include <nanomsg/nn.h>
#include <nanomsg/pipeline.h>
#include <nanomsg/reqrep.h>
#include <spdlog/spdlog.h>
#include <atomic>
#include <chrono>
#include <cstring>
#include <iostream>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
#include "imscope_common.h"
#include "imscope_internal.h"

#ifdef __cpp_lib_hardware_interference_size
using std::hardware_destructive_interference_size;
#else
// 64 bytes on x86-64 │ L1_CACHE_BYTES │ L1_CACHE_SHIFT │ __cacheline_aligned │
// ...
constexpr std::size_t hardware_destructive_interference_size = 64;
#endif

template <typename T>
struct PaddedAtomic {
  std::atomic<T> value;
  char padding[hardware_destructive_interference_size - sizeof(std::atomic<T>)];

  PaddedAtomic() = default;

  PaddedAtomic(T initial_value) : value(initial_value) {}

  PaddedAtomic(const PaddedAtomic& other) : value(other.value.load()) {}

  T load() const noexcept { return value.load(); }

  void store(T desired) noexcept { value.store(desired); }

  T fetch_add(T arg) noexcept { return value.fetch_add(arg); }
};

typedef struct {
  std::string name;
  scope_type_t type;
} scope_info_t;

class ImscopeProducer {
  std::string control_address;
  std::string data_address;
  std::string announce_address;
  std::string name;
  std::thread control_thread_handle;
  std::thread announce_thread_handle;

  std::vector<scope_info_t> configured_scopes;
  std::vector<PaddedAtomic<int>> credits;

  std::map<pthread_t, int> push_sockets;

  void start_control_thread() {
    control_thread_handle = std::thread([this]() {
      pthread_setname_np(pthread_self(), "imscope_ctrl");
      spdlog::debug(
          "ImscopeProducer: Control thread started. Listening for "
          "credits on {}",
          this->control_address);
      const char* control_address = this->control_address.c_str();
      int socket = create_nn_pull_socket(control_address);

      while (1) {
        char* msg_buf = NULL;
        int bytes = nn_recv(socket, &msg_buf, NN_MSG, 0);
        if (bytes < 0) {
          nn_freemsg(msg_buf);
          continue;
        }
        spdlog::debug("ImscopeProducer: Received control message");
        control_msg_t* msg = (control_msg_t*)msg_buf;
        this->credits[msg->id].fetch_add(msg->credits);
      }
    });
  }

  void start_announce_thread() {
    announce_thread_handle = std::thread([this]() {
      pthread_setname_np(pthread_self(), "imscope_announce");
      const char* announce_address = this->announce_address.c_str();
      int rep_sock = nn_socket(AF_SP, NN_REP);
      nn_bind(rep_sock, announce_address);

      while (1) {
        char* buf = NULL;
        int bytes = nn_recv(rep_sock, &buf, NN_MSG, 0);
        if (bytes < 0) {
          nn_freemsg(buf);
          continue;
        }
        announce_request_t* req = (announce_request_t*)buf;
        if (req->magic != ANNOUNCE_MSG_ID) {
          nn_freemsg(buf);
          continue;
        }
        size_t size = sizeof(announce_response_t) +
                      configured_scopes.size() * sizeof(imscope_scope_config_t);
        char* msg_buf = (char*)nn_allocmsg(size, 0);

        // Prepare protocol description
        announce_response_t* msg = (announce_response_t*)msg_buf;
        msg->num_scopes = configured_scopes.size();
        strncpy(msg->data_address, this->data_address.c_str(),
                sizeof(msg->data_address) - 1);
        strncpy(msg->control_address, this->control_address.c_str(),
                sizeof(msg->control_address) - 1);
        strncpy(msg->name, this->name.c_str(), sizeof(msg->name) - 1);
        for (size_t i = 0; i < configured_scopes.size(); i++) {
          strncpy(msg->scopes[i].name, configured_scopes[i].name.c_str(),
                  MAX_SCOPE_NAME_LEN - 1);
          msg->scopes[i].type = configured_scopes[i].type;
        }

        spdlog::debug(
            "ImscopeProducer: Announced {} scopes to consumer."
            "Data address: {} Control address: {}",
            configured_scopes.size(), this->data_address,
            this->control_address);
        nn_send(rep_sock, msg_buf, size, 0);
        nn_freemsg(buf);
      }
    });
  }

  int get_socket_for_thread() {
    pthread_t tid = pthread_self();
    if (push_sockets.count(tid) == 0) {
      push_sockets[tid] = create_nn_push_socket(data_address.c_str());
    }
    return push_sockets[tid];
  }

 public:
  ImscopeProducer() {}

  void connect(const char* control_address, const char* data_address,
               const char* announce_address, const char* name) {
    this->control_address = control_address;
    this->data_address = data_address;
    this->announce_address = announce_address;
    this->name = name;
    credits = std::vector<PaddedAtomic<int>>(configured_scopes.size(),
                                             PaddedAtomic<int>(0));
    start_control_thread();
    start_announce_thread();
  }

  int add_scope(const char* name, scope_type_t type) {
    scope_info_t scope_info = {name, type};
    configured_scopes.push_back(scope_info);
    return 0;  // Success
  }

  void send_scope_data(uint32_t* data, int id, size_t num_samples, int frame,
                       int slot) {
    if (credits[id].load() < 0) {
      return;
    }
    spdlog::debug(
        "ImscopeProducer: Sending {} samples for scope id {} (frame {}, slot "
        "{})",
        num_samples, id, frame, slot);
    auto start = std::chrono::high_resolution_clock::now();
    int socket = get_socket_for_thread();
    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    char* msg_buf = (char*)nn_allocmsg(size, 0);
    scope_msg_t* msg = (scope_msg_t*)msg_buf;
    msg->id = id;
    msg->meta.frame = frame;
    msg->data_size = num_samples * sizeof(uint32_t);
    memcpy((void*)(msg + 1), data, sizeof(uint32_t) * num_samples);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();
    nn_send(socket, &msg_buf, NN_MSG, NN_DONTWAIT);
    credits[id].fetch_add(-1);
  }
};

static ImscopeProducer* instance = nullptr;

extern "C" int imscope_init_producer(const char* control_address,
                                     const char* data_address,
                                     const char* announce_address,
                                     const char* name,
                                     imscope_scope_desc_t* scopes,
                                     size_t num_scopes) {
  if (instance == nullptr) {
    instance = new ImscopeProducer();
  }
  for (size_t i = 0; i < num_scopes; ++i) {
    instance->add_scope(scopes[i].name, scopes[i].type);
  }
  instance->connect(control_address, data_address, announce_address, name);
  return 0;  // Success
}

extern "C" int imscope_send_data(uint32_t* data, int id, size_t num_samples,
                                 int frame, int slot) {
  if (instance == nullptr) {
    return -1;  // Not initialized
  }
  instance->send_scope_data(data, id, num_samples, frame, slot);
  return 0;  // Success
}