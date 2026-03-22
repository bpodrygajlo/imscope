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
#include <memory>
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
  std::thread announce_thread_handle;
  std::atomic<bool> stop_announce_thread_flag{false};

  std::vector<scope_info_t> configured_scopes;

  nng_socket data_socket;

  struct ScopeCtx {
    nng_ctx ctx;
    nng_aio* send_aio;
    nng_aio* recv_aio;
    std::atomic<bool> busy;
    nng_msg* acquired_msg;
    ImscopeProducer* parent;
    int id;

    ScopeCtx(nng_socket socket, ImscopeProducer* p, int idx)
        : parent(p), id(idx), busy(false), acquired_msg(nullptr) {
      nng_ctx_open(&ctx, socket);
      nng_aio_alloc(&send_aio, send_callback, this);
      nng_aio_alloc(&recv_aio, recv_callback, this);
    }

    ~ScopeCtx() {
      nng_aio_free(send_aio);
      nng_aio_free(recv_aio);
      nng_ctx_close(ctx);
      if (acquired_msg) {
        nng_msg_free(acquired_msg);
      }
    }

    static void send_callback(void* arg) {
      auto self = static_cast<ScopeCtx*>(arg);
      int rv = nng_aio_result(self->send_aio);
      if (rv != 0) {
        spdlog::error("ImscopeProducer: send failed for scope {}: {}", self->id,
                      nng_strerror(rv));
        self->busy.store(false);
      } else {
        // Send finished, start receiving REP
        nng_ctx_recv(self->ctx, self->recv_aio);
      }
    }

    static void recv_callback(void* arg) {
      auto self = static_cast<ScopeCtx*>(arg);
      int rv = nng_aio_result(self->recv_aio);
      if (rv != 0) {
        spdlog::error("ImscopeProducer: recv failed for scope {}: {}", self->id,
                      nng_strerror(rv));
      } else {
        nng_msg* msg = nng_aio_get_msg(self->recv_aio);
        if (msg) {
          nng_msg_free(msg);
        }
        spdlog::debug("ImscopeProducer: Received REP for scope id {}",
                      self->id);
      }
      self->busy.store(false);
    }
  };

  std::vector<std::unique_ptr<ScopeCtx>> scope_contexts;

  void start_announce_thread() {
    announce_thread_handle = std::thread([this]() {
      pthread_setname_np(pthread_self(), "imscope_announce");
      const char* announce_address = this->announce_address.c_str();
      nng_socket rep_sock;
      nng_rep0_open(&rep_sock);
      int rv = nng_listen(rep_sock, announce_address, NULL, 0);
      if (rv != 0) {
        spdlog::error("ImscopeProducer: Failed to listen on {}: {}",
                      announce_address, nng_strerror(rv));
        return;
      }

      while (!stop_announce_thread_flag.load()) {
        nng_msg* req_msg = NULL;
        // Use a timeout for recv to check stop flag
        nng_socket_set_ms(rep_sock, NNG_OPT_RECVTIMEO, 100);
        int rv = nng_recvmsg(rep_sock, &req_msg, 0);
        if (rv != 0) {
          continue;
        }
        announce_request_t* req = (announce_request_t*)nng_msg_body(req_msg);
        if (req->magic != ANNOUNCE_MSG_ID) {
          nng_msg_free(req_msg);
          continue;
        }
        size_t size = sizeof(announce_response_t) +
                      configured_scopes.size() * sizeof(imscope_scope_config_t);

        nng_msg* res_msg;
        nng_msg_alloc(&res_msg, size);

        // Prepare protocol description
        announce_response_t* msg = (announce_response_t*)nng_msg_body(res_msg);
        msg->num_scopes = configured_scopes.size();
        strncpy(msg->data_address, this->data_address.c_str(),
                sizeof(msg->data_address) - 1);
        strncpy(msg->name, this->name.c_str(), sizeof(msg->name) - 1);
        for (size_t i = 0; i < configured_scopes.size(); i++) {
          strncpy(msg->scopes[i].name, configured_scopes[i].name.c_str(),
                  MAX_SCOPE_NAME_LEN - 1);
          msg->scopes[i].type = configured_scopes[i].type;
        }

        spdlog::debug(
            "ImscopeProducer: Announced {} scopes to consumer."
            "Data address: {} Announce address: {}",
            configured_scopes.size(), this->data_address,
            this->announce_address);
        nng_sendmsg(rep_sock, res_msg, 0);
        nng_msg_free(req_msg);
      }
    });
  }

 public:
  ImscopeProducer() {}

  ~ImscopeProducer() {
    if (announce_thread_handle.joinable()) {
      stop_announce_thread_flag.store(true);
      announce_thread_handle.join();
    }
    nng_close(data_socket);
  }

  void clear_scopes() { configured_scopes.clear(); }

  void connect(const char* data_address, const char* announce_address,
               const char* name) {
    this->data_address = data_address;
    this->announce_address = announce_address;
    this->name = name;

    // Clear previous state if any
    scope_contexts.clear();

    this->data_socket = create_nng_req_socket(data_address);

    for (size_t i = 0; i < configured_scopes.size(); i++) {
      scope_contexts.push_back(
          std::make_unique<ScopeCtx>(this->data_socket, this, i));
    }

    if (announce_thread_handle.joinable()) {
      stop_announce_thread_flag.store(true);
      announce_thread_handle.join();
    }
    stop_announce_thread_flag.store(false);
    start_announce_thread();
  }

  imscope_return_t add_scope(const char* name, scope_type_t type) {
    scope_info_t scope_info = {name, type};
    configured_scopes.push_back(scope_info);
    return IMSCOPE_SUCCESS;
  }

  imscope_return_t send_scope_data(uint32_t* data, int id, size_t num_samples,
                                   int frame, int slot, uint64_t timestamp) {
    if (id >= (int)scope_contexts.size()) {
      return IMSCOPE_ERROR_INVALID_ID;
    }
    if (scope_contexts[id]->busy.exchange(true)) {
      return IMSCOPE_ERROR_BUSY;
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

    nng_aio_set_msg(scope_contexts[id]->send_aio, msg_obj);
    nng_ctx_send(scope_contexts[id]->ctx, scope_contexts[id]->send_aio);
    return IMSCOPE_SUCCESS;
  }

  void* acquire_buffer(int id, size_t num_samples) {
    if (id >= (int)scope_contexts.size()) {
      return nullptr;
    }
    auto& ctx = scope_contexts[id];
    if (ctx->busy.exchange(true)) {
      return nullptr;
    }

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    if (nng_msg_alloc(&ctx->acquired_msg, size) != 0) {
      ctx->busy.store(false);
      return nullptr;
    }

    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(ctx->acquired_msg);
    return (void*)(msg + 1);
  }

  imscope_return_t commit_buffer(int id, size_t num_samples, int frame,
                                 int slot, uint64_t timestamp) {
    if (id >= (int)scope_contexts.size()) {
      return IMSCOPE_ERROR_INVALID_ID;
    }
    auto& ctx = scope_contexts[id];
    if (ctx->acquired_msg == nullptr) {
      return IMSCOPE_ERROR_NOT_INITIALIZED;
    }

    auto start = std::chrono::high_resolution_clock::now();
    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(ctx->acquired_msg);
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    nng_aio_set_msg(ctx->send_aio, ctx->acquired_msg);
    ctx->acquired_msg = nullptr;
    nng_ctx_send(ctx->ctx, ctx->send_aio);
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
