/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <nng/nng.h>
#include <nng/protocol/reqrep0/rep.h>
#include <nng/protocol/reqrep0/req.h>
#include "imscope_common.h"
#include "imscope_internal.h"

#include <spdlog/spdlog.h>
#include <atomic>
#include <chrono>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <queue>
#include <string>
#include <thread>
#include <vector>

#include "imscope_producer.h"

typedef struct {
  std::string name;
  scope_type_t type;
} scope_info_t;

class ImscopeProducer {
  std::string data_address;
  std::string announce_address;
  std::string name;
  nng_socket announce_socket = NNG_SOCKET_INITIALIZER;

  std::vector<scope_info_t> configured_scopes;

  nng_socket data_socket = NNG_SOCKET_INITIALIZER;

  struct ScopeCtx {
    nng_ctx ctx;
    nng_aio* send_aio;
    nng_aio* recv_aio;
    std::atomic<bool> req_received;
    ImscopeProducer* parent;
    int id;

    ScopeCtx(nng_socket socket, ImscopeProducer* p)
        : parent(p), id(-1), req_received(false) {
      nng_ctx_open(&ctx, socket);
      nng_aio_alloc(&send_aio, NULL, NULL);
      nng_aio_alloc(&recv_aio, recv_callback, this);
      // Start waiting for the first request
      nng_ctx_recv(ctx, recv_aio);
    }

    ~ScopeCtx() {
      nng_aio_free(send_aio);
      nng_aio_free(recv_aio);
      nng_ctx_close(ctx);
    }

    static void recv_callback(void* arg) {
      auto self = static_cast<ScopeCtx*>(arg);
      int rv = nng_aio_result(self->recv_aio);
      if (rv == 0) {
        nng_msg* msg = nng_aio_get_msg(self->recv_aio);
        if (nng_msg_len(msg) >= sizeof(scope_request_t)) {
          scope_request_t* req = (scope_request_t*)nng_msg_body(msg);
          if (req->magic == SCOPE_REQ_MSG_ID) {
            self->id = req->scope_id;
            std::lock_guard<std::mutex> lock(
                self->parent->active_requests_mutex);
            self->parent->active_requests[self->id] = self;
            self->req_received.store(true);
            return;  // Don't restart recv yet, wait for send
          }
        }
        // If message is invalid, just restart recv
        nng_msg_free(msg);
        nng_ctx_recv(self->ctx, self->recv_aio);
      } else if (rv != (int)NNG_ECLOSED) {
        // Restart on error
        nng_ctx_recv(self->ctx, self->recv_aio);
      }
    }
  };

  struct AnnounceCtx {
    nng_ctx ctx;
    nng_aio* send_aio;
    nng_aio* recv_aio;
    ImscopeProducer* parent;

    AnnounceCtx(nng_socket socket, ImscopeProducer* p) : parent(p) {
      nng_ctx_open(&ctx, socket);
      nng_aio_alloc(&send_aio, NULL, NULL);
      nng_aio_alloc(&recv_aio, announce_callback, this);
      // Start waiting for the first announce request
      nng_ctx_recv(ctx, recv_aio);
    }

    ~AnnounceCtx() {
      nng_aio_free(send_aio);
      nng_aio_free(recv_aio);
      nng_ctx_close(ctx);
    }

    static void announce_callback(void* arg) {
      auto self = static_cast<AnnounceCtx*>(arg);
      int rv = nng_aio_result(self->recv_aio);
      if (rv == 0) {
        nng_msg* req_msg = nng_aio_get_msg(self->recv_aio);
        if (nng_msg_len(req_msg) >= sizeof(announce_request_t)) {
          announce_request_t* req = (announce_request_t*)nng_msg_body(req_msg);
          if (req->magic == ANNOUNCE_MSG_ID) {
            size_t size = sizeof(announce_response_t) +
                          self->parent->configured_scopes.size() *
                              sizeof(imscope_scope_config_t);

            nng_msg* res_msg;
            nng_msg_alloc(&res_msg, size);

            // Prepare protocol description
            announce_response_t* msg =
                (announce_response_t*)nng_msg_body(res_msg);
            msg->num_scopes = self->parent->configured_scopes.size();
            strncpy(msg->data_address, self->parent->data_address.c_str(),
                    sizeof(msg->data_address) - 1);
            strncpy(msg->name, self->parent->name.c_str(),
                    sizeof(msg->name) - 1);
            for (size_t i = 0; i < self->parent->configured_scopes.size();
                 i++) {
              strncpy(msg->scopes[i].name,
                      self->parent->configured_scopes[i].name.c_str(),
                      MAX_SCOPE_NAME_LEN - 1);
              msg->scopes[i].type = self->parent->configured_scopes[i].type;
            }

            spdlog::debug(
                "ImscopeProducer: Announced {} scopes to consumer."
                "Data address: {} Announce address: {}",
                self->parent->configured_scopes.size(),
                self->parent->data_address, self->parent->announce_address);

            // Send response asynchronously
            nng_aio_set_msg(self->send_aio, res_msg);
            nng_ctx_send(self->ctx, self->send_aio);
          }
        }
        nng_msg_free(req_msg);
        nng_ctx_recv(self->ctx, self->recv_aio);
      } else if (rv != (int)NNG_ECLOSED) {
        // Restart on error
        nng_ctx_recv(self->ctx, self->recv_aio);
      }
    }
  };

  std::vector<std::unique_ptr<ScopeCtx>> workers;
  std::unique_ptr<AnnounceCtx> announce_handler;
  std::map<int, ScopeCtx*> active_requests;
  std::mutex active_requests_mutex;

 public:
  ImscopeProducer() {}

  ~ImscopeProducer() {
    announce_handler.reset();
    if (nng_socket_id(announce_socket) > 0) {
      nng_close(announce_socket);
    }
    workers.clear();
    if (nng_socket_id(data_socket) > 0) {
      nng_close(data_socket);
    }
    for (auto& pair : acquired_msgs) {
      if (pair.second)
        nng_msg_free(pair.second);
    }
  }

  void clear_scopes() { configured_scopes.clear(); }

  void connect(const char* data_address, const char* announce_address,
               const char* name) {
    this->data_address = data_address;
    this->announce_address = announce_address;
    this->name = name;

    // Clear previous state if any
    workers.clear();
    active_requests.clear();
    acquired_msgs.clear();
    if (nng_socket_id(data_socket) > 0) {
      nng_close(data_socket);
    }
    if (nng_socket_id(announce_socket) > 0) {
      nng_close(announce_socket);
    }

    this->data_socket = create_nng_rep_socket(data_address);

    for (size_t i = 0; i < configured_scopes.size(); i++) {
      workers.push_back(std::make_unique<ScopeCtx>(this->data_socket, this));
    }

    nng_rep0_open(&announce_socket);
    int rv = nng_listen(announce_socket, announce_address, NULL, 0);
    if (rv != 0) {
      spdlog::error("ImscopeProducer: Failed to listen on {}: {}",
                    announce_address, nng_strerror(rv));
      return;
    }

    announce_handler =
        std::make_unique<AnnounceCtx>(this->announce_socket, this);
  }

  imscope_return_t add_scope(const char* name, scope_type_t type) {
    scope_info_t scope_info = {name, type};
    configured_scopes.push_back(scope_info);
    return IMSCOPE_SUCCESS;
  }

  imscope_return_t send_scope_data(uint32_t* data, int id, size_t num_samples,
                                   int frame, int slot, uint64_t timestamp) {
    ScopeCtx* worker = nullptr;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return IMSCOPE_ERROR_BUSY;
      }
      worker = it->second;
      active_requests.erase(it);
    }

    worker->req_received.store(false);

    nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
    if (req_msg) {
      nng_msg_free(req_msg);
    }

    auto start = std::chrono::high_resolution_clock::now();

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    nng_msg* msg_obj;
    nng_msg_alloc(&msg_obj, size);
    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(msg_obj);
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    memcpy((void*)(msg + 1), data, sizeof(uint32_t) * num_samples);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    nng_aio_set_msg(worker->send_aio, msg_obj);
    nng_ctx_send(worker->ctx, worker->send_aio);
    nng_aio_wait(worker->send_aio);
    int rv = nng_aio_result(worker->send_aio);

    // Restart recv for next REQ
    nng_ctx_recv(worker->ctx, worker->recv_aio);

    if (rv != 0) {
      return IMSCOPE_ERROR_INTERNAL;
    }

    return IMSCOPE_SUCCESS;
  }

  std::map<int, nng_msg*> acquired_msgs;

  void* acquire_buffer(int id, size_t num_samples) {
    ScopeCtx* worker = nullptr;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return nullptr;
      }
      worker = it->second;
    }

    // Consume the request
    worker->req_received.store(false);
    nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
    if (req_msg) {
      nng_msg_free(req_msg);
    }

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    if (nng_msg_alloc(&acquired_msgs[id], size) != 0) {
      // Restart recv on failure
      nng_ctx_recv(worker->ctx, worker->recv_aio);
      return nullptr;
    }

    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(acquired_msgs[id]);
    msg->id = id;
    return (void*)(msg + 1);
  }

  imscope_return_t commit_buffer(int id, size_t num_samples, int frame,
                                 int slot, uint64_t timestamp) {
    ScopeCtx* worker = nullptr;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return IMSCOPE_ERROR_BUSY;  // Should not happen if acquired
      }
      worker = it->second;
      active_requests.erase(it);
    }

    if (acquired_msgs[id] == nullptr) {
      nng_ctx_recv(worker->ctx, worker->recv_aio);
      return IMSCOPE_ERROR_NOT_INITIALIZED;
    }

    auto start = std::chrono::high_resolution_clock::now();
    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(acquired_msgs[id]);
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    nng_aio_set_msg(worker->send_aio, acquired_msgs[id]);
    acquired_msgs[id] = nullptr;
    nng_ctx_send(worker->ctx, worker->send_aio);
    nng_aio_wait(worker->send_aio);
    int rv = nng_aio_result(worker->send_aio);

    // Restart recv
    nng_ctx_recv(worker->ctx, worker->recv_aio);

    if (rv != 0) {
      return IMSCOPE_ERROR_INTERNAL;
    }

    return IMSCOPE_SUCCESS;
  }
};

static ImscopeProducer* instance = nullptr;

extern "C" imscope_return_t imscope_init_producer(const char* data_address,
                                                  const char* announce_address,
                                                  const char* name,
                                                  imscope_scope_desc_t* scopes,
                                                  size_t num_scopes) {
  if (instance == nullptr) {
    instance = new ImscopeProducer();
  }
  instance->clear_scopes();
  for (size_t i = 0; i < num_scopes; ++i) {
    instance->add_scope(scopes[i].name, scopes[i].type);
  }
  instance->connect(data_address, announce_address, name);
  return IMSCOPE_SUCCESS;
}

extern "C" imscope_return_t imscope_try_send_data(uint32_t* data, int id,
                                                  size_t num_samples, int frame,
                                                  int slot,
                                                  uint64_t timestamp) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->send_scope_data(data, id, num_samples, frame, slot,
                                   timestamp);
}

extern "C" void* imscope_acquire_send_buffer(int id, size_t num_samples) {
  if (instance == nullptr) {
    return nullptr;
  }
  return instance->acquire_buffer(id, num_samples);
}

extern "C" imscope_return_t imscope_commit_send_buffer(int id,
                                                       size_t num_samples,
                                                       int frame, int slot,
                                                       uint64_t timestamp) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->commit_buffer(id, num_samples, frame, slot, timestamp);
}

extern "C" void imscope_cleanup_producer() {
  if (instance != nullptr) {
    delete instance;
    instance = nullptr;
  }
}
