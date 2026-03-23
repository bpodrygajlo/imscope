/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include "imscope_consumer.h"
#include <nng/nng.h>
#include <nng/protocol/pipeline0/pull.h>
#include <nng/protocol/reqrep0/req.h>
#include <spdlog/spdlog.h>
#include <cstddef>
#include <mutex>
#include <queue>
#include <thread>
#include <vector>
#include "imscope_common.h"
#include "imscope_internal.h"

class ContextWorker {
 public:
  nng_ctx ctx;
  nng_aio* recv_aio;
  nng_aio* rep_aio;
  ImscopeConsumer* parent;

  ContextWorker(nng_socket socket, ImscopeConsumer* p) : parent(p) {
    nng_ctx_open(&ctx, socket);
    nng_aio_alloc(&recv_aio, recv_callback, this);
    nng_aio_alloc(&rep_aio, rep_callback, this);
  }

  ~ContextWorker() {
    nng_aio_free(recv_aio);
    nng_aio_free(rep_aio);
    nng_ctx_close(ctx);
  }

  void start_recv() { nng_ctx_recv(ctx, recv_aio); }

  static void recv_callback(void* arg) {
    auto self = static_cast<ContextWorker*>(arg);
    int rv = nng_aio_result(self->recv_aio);
    if (rv != 0) {
      if (rv != NNG_ECLOSED) {
        spdlog::error("ImscopeConsumer: recv aio failed: {}", nng_strerror(rv));
        self->start_recv();
      }
      return;
    }

    nng_msg* msg_obj = nng_aio_get_msg(self->recv_aio);
    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(msg_obj);
    int scope_id = msg->id;

    spdlog::debug(
        "ImscopeConsumer: Received scope message for scope id {} (frame "
        "{}, slot {})",
        scope_id, msg->meta.frame, msg->meta.slot);

    auto& buffer = *self->parent->scope_buffers[scope_id];
    std::unique_lock<std::mutex> lock(buffer.mutex, std::try_to_lock);
    if (!lock.owns_lock()) {
      spdlog::warn(
          "ImscopeConsumer: Buffer busy for scope {}, discarding message",
          scope_id);
      nng_msg_free(msg_obj);
      return;
    }

    if (buffer.msg != nullptr) {
      spdlog::warn(
          "ImscopeConsumer: Buffer full for scope {}, discarding message",
          scope_id);
      nng_msg_free(msg_obj);
      return;
    }

    buffer.msg = make_nng_msg_ptr(msg_obj, self);
    buffer.version++;
  }

  static void rep_callback(void* arg) {
    auto self = static_cast<ContextWorker*>(arg);
    int rv = nng_aio_result(self->rep_aio);
    if (rv != 0) {
      spdlog::error("ImscopeConsumer: rep aio failed: {}", nng_strerror(rv));
    }
    // Successfully sent REP, now wait for next REQ
    self->start_recv();
  }

  void send_rep() {
    nng_msg* ack;
    nng_msg_alloc(&ack, 0);
    nng_aio_set_msg(rep_aio, ack);
    nng_ctx_send(ctx, rep_aio);
  }
};

NngMsgPtr make_nng_msg_ptr(nng_msg* msg, ContextWorker* worker) {
  if (!msg)
    return nullptr;
  return NngMsgPtr(nng_msg_body(msg), [msg, worker](void*) {
    nng_msg_free(msg);
    worker->send_rep();
  });
}

ImscopeConsumer::ImscopeConsumer(const char* data_address,
                                 const char* announce_address, int num_scopes,
                                 imscope_scope_config_t* scopes,
                                 const char* name)
    : data_address(data_address),
      announce_address(announce_address),
      configured_scopes(scopes, scopes + num_scopes),
      name(name) {
  for (size_t i = 0; i < configured_scopes.size(); i++) {
    scope_buffers.push_back(std::make_unique<ScopeBuffer>());
  }

  this->data_socket = create_nng_rep_socket(data_address);

  // Create twice as many workers as scopes to handle potential overlaps
  for (size_t i = 0; i < (size_t)num_scopes * 2; i++) {
    auto worker = std::make_unique<ContextWorker>(this->data_socket, this);
    worker->start_recv();
    workers.push_back(std::move(worker));
  }
}

ImscopeConsumer::~ImscopeConsumer() {}

ImscopeConsumer* ImscopeConsumer::connect(const char* announce_address) {
  nng_socket req_sock;
  int rv = nng_req0_open(&req_sock);
  if (rv != 0) {
    return nullptr;
  }
  rv = nng_dial(req_sock, announce_address, NULL, NNG_FLAG_NONBLOCK);
  if (rv != 0) {
    nng_close(req_sock);
    return nullptr;
  }

  nng_duration timeout = 2000;  // milliseconds
  nng_socket_set_ms(req_sock, NNG_OPT_RECVTIMEO, timeout);

  nng_msg* req_msg;
  rv = nng_msg_alloc(&req_msg, sizeof(announce_request_t));
  if (rv != 0) {
    FatalError("Failed to allocate memory for announce message");
  }

  announce_request_t* announce_msg = (announce_request_t*)nng_msg_body(req_msg);
  announce_msg->magic = ANNOUNCE_MSG_ID;
  nng_sendmsg(req_sock, req_msg, 0);

  nng_msg* res_msg;
  rv = nng_recvmsg(req_sock, &res_msg, 0);
  if (rv != 0) {
    nng_close(req_sock);
    return nullptr;
  }

  announce_response_t* response = (announce_response_t*)nng_msg_body(res_msg);
  print_announce_response(response);
  auto consumer = new ImscopeConsumer(response->data_address, announce_address,
                                      response->num_scopes, response->scopes,
                                      response->name);
  nng_msg_free(res_msg);
  nng_close(req_sock);
  return consumer;
}

NngMsgPtr ImscopeConsumer::try_collect_scope_msg(int scope_id, int& version) {
  auto& buffer = *scope_buffers[scope_id];
  std::unique_lock<std::mutex> lock(buffer.mutex);
  if (buffer.msg != nullptr && version != buffer.version) {
    version = buffer.version;
    NngMsgPtr msg = std::move(buffer.msg);
    buffer.msg = nullptr;
    return msg;
  }
  return nullptr;
}
