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

struct ImscopeConsumer::ScopeCtx {
  nng_ctx ctx;
  nng_aio* send_aio;
  nng_aio* recv_aio;

  ScopeCtx(nng_socket socket) {
    nng_ctx_open(&ctx, socket);
    nng_aio_alloc(&send_aio, NULL, NULL);
    nng_aio_alloc(&recv_aio, NULL, NULL);
  }

  ~ScopeCtx() {
    nng_aio_free(send_aio);
    nng_aio_free(recv_aio);
    nng_ctx_close(ctx);
  }
};

NngMsgPtr make_nng_msg_ptr(nng_msg* msg) {
  if (!msg)
    return nullptr;
  return NngMsgPtr(nng_msg_body(msg), [msg](void*) { nng_msg_free(msg); });
}

ImscopeConsumer::ImscopeConsumer(const char* data_address, int num_scopes,
                                 imscope_scope_config_t* scopes,
                                 const char* name)
    : data_address(data_address),
      configured_scopes(scopes, scopes + num_scopes),
      name(name) {
  this->data_socket = create_nng_req_socket(data_address);

  for (size_t i = 0; i < configured_scopes.size(); i++) {
    scope_contexts.push_back(std::make_unique<ScopeCtx>(this->data_socket));
  }
}

ImscopeConsumer::~ImscopeConsumer() {
  nng_close(data_socket);
}

imscope_return_t ImscopeConsumer::request_data(int scope_id) {
  if (scope_id >= (int)scope_contexts.size()) {
    return IMSCOPE_ERROR_INVALID_ID;
  }
  auto& sc = scope_contexts[scope_id];

  // Only allow request if we are not already waiting for one
  if (nng_aio_result(sc->recv_aio) == -1) {
    return IMSCOPE_SUCCESS;
  }

  nng_msg* msg;
  nng_msg_alloc(&msg, sizeof(scope_request_t));
  scope_request_t* req = (scope_request_t*)nng_msg_body(msg);
  req->magic = SCOPE_REQ_MSG_ID;
  req->scope_id = scope_id;

  nng_aio_set_msg(sc->send_aio, msg);
  nng_ctx_send(sc->ctx, sc->send_aio);
  nng_aio_wait(sc->send_aio);

  // Restart recv immediately to wait for the response
  nng_ctx_recv(sc->ctx, sc->recv_aio);
  return IMSCOPE_SUCCESS;
}

NngMsgPtr ImscopeConsumer::try_collect_scope_msg(int scope_id, int& version) {
  if (scope_id >= (int)scope_contexts.size()) {
    return nullptr;
  }
  auto& sc = scope_contexts[scope_id];

  // Check if REQ is ready
  // nng_aio_result returns -1 (NNG_EINPROGRESS) if the operation is still pending.
  // If it's 0, it means the previous recv completed. If it's anything else, it's an error.
  int rv = nng_aio_result(sc->recv_aio);
  if (rv == -1) {  // In progress, no message yet
    return nullptr;
  } else if (rv != 0) {  // An error occurred
    spdlog::error(
        "ImscopeConsumer: nng_aio_result on recv_aio returned error: {}",
        nng_strerror(rv));
    return nullptr;
  }

  nng_msg* msg = nng_aio_get_msg(sc->recv_aio);
  version++;
  return make_nng_msg_ptr(msg);
}

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
  auto consumer =
      new ImscopeConsumer(response->data_address, response->num_scopes,
                          response->scopes, response->name);
  nng_msg_free(res_msg);
  nng_close(req_sock);
  return consumer;
}

bool ImscopeConsumer::try_collect_iq(int scope_id, std::vector<int16_t>& real,
                                     std::vector<int16_t>& imag) {
  int version = 0;
  auto msg_ptr = try_collect_scope_msg(scope_id, version);
  if (!msg_ptr) {
    return false;
  }

  scope_msg_t* msg = static_cast<scope_msg_t*>(msg_ptr.get());
  size_t num_samples = msg->data_size / 4;  // Assuming 32-bit (16+16) samples
  uint32_t* data = reinterpret_cast<uint32_t*>(msg + 1);

  real.clear();
  imag.clear();
  real.reserve(num_samples);
  imag.reserve(num_samples);

  for (size_t i = 0; i < num_samples; ++i) {
    real.push_back(static_cast<int16_t>(data[i] & 0xFFFF));
    imag.push_back(static_cast<int16_t>((data[i] >> 16) & 0xFFFF));
  }

  return true;
}

bool ImscopeConsumer::try_collect_real(int scope_id,
                                       std::vector<int16_t>& real) {
  int version = 0;
  auto msg_ptr = try_collect_scope_msg(scope_id, version);
  if (!msg_ptr) {
    return false;
  }

  scope_msg_t* msg = static_cast<scope_msg_t*>(msg_ptr.get());
  size_t num_samples = msg->data_size / 2;
  int16_t* data = reinterpret_cast<int16_t*>(msg + 1);

  real.clear();
  real.reserve(num_samples);
  for (size_t i = 0; i < num_samples; ++i) {
    real.push_back(data[i]);
  }

  return true;
}
