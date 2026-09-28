/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include "imscope_consumer.h"

#include <spdlog/spdlog.h>
#include <cstddef>
#include <cstring>
#include <mutex>
#include <string>
#include <variant>
#include <vector>

#include "imscope_common.h"
#include "imscope_internal.h"

// Wraps a heap-allocated byte buffer (a Zenoh reply's payload) in a
// `NngMsgPtr`, using `shared_ptr`'s aliasing constructor so the returned
// pointer refers directly to the buffer's first byte (where the wire
// `scope_msg_t` starts) while the backing `vector` is what actually keeps
// the memory alive.
static NngMsgPtr make_owned_msg_ptr(std::vector<uint8_t>&& bytes) {
  auto storage = std::make_shared<std::vector<uint8_t>>(std::move(bytes));
  void* data_ptr = storage->data();
  return NngMsgPtr(storage, data_ptr);
}

ImscopeConsumer::ImscopeConsumer(zenoh::Session&& session_in, int num_scopes,
                                 imscope_scope_config_t* scopes,
                                 const char* name)
    : name(name),
      session(std::move(session_in)),
      configured_scopes(scopes, scopes + num_scopes),
      scope_pending(num_scopes) {}

ImscopeConsumer::~ImscopeConsumer() = default;

imscope_return_t ImscopeConsumer::request_data(int scope_id) {
  std::lock_guard<std::mutex> lock(scopes_mutex);
  if (scope_id < 0 || scope_id >= (int)configured_scopes.size()) {
    return IMSCOPE_ERROR_INVALID_ID;
  }

  // Only allow a new request if we are not already waiting for one --
  // mirrors the NNG version's `nng_aio_result(sc->recv_aio) == -1` "still in
  // progress" guard.
  if (scope_pending[scope_id].has_value()) {
    return IMSCOPE_SUCCESS;
  }

  if (!session.has_value()) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }

  std::string key_expr = "imscope/" + name + "/data/" + std::to_string(scope_id);

  scope_request_t req;
  req.magic = SCOPE_REQ_MSG_ID;
  req.scope_id = scope_id;
  std::vector<uint8_t> req_bytes(sizeof(req));
  std::memcpy(req_bytes.data(), &req, sizeof(req));

  zenoh::Session::GetOptions options = zenoh::Session::GetOptions::create_default();
  options.payload = zenoh::Bytes(std::move(req_bytes));
  options.timeout_ms = 1000;

  // `session->get()` sends the query and returns immediately -- replies are
  // delivered asynchronously into the returned channel by Zenoh's own
  // runtime, exactly like `nng_ctx_send()` firing off a request without
  // blocking for the response. `try_collect_scope_msg()` below polls this
  // channel the same way it used to poll `nng_aio_result()`.
  try {
    auto handler =
        session->get(key_expr, "", zenoh::channels::FifoChannel(1), std::move(options));
    scope_pending[scope_id].emplace(std::move(handler));
  } catch (const zenoh::ZException& e) {
    spdlog::error("ImscopeConsumer: failed to send data request for scope {}: {}",
                  scope_id, e.what());
    return IMSCOPE_ERROR_INTERNAL;
  }

  return IMSCOPE_SUCCESS;
}

NngMsgPtr ImscopeConsumer::try_collect_scope_msg(int scope_id, int& version) {
  std::lock_guard<std::mutex> lock(scopes_mutex);
  if (scope_id < 0 || scope_id >= (int)configured_scopes.size()) {
    return nullptr;
  }
  if (!scope_pending[scope_id].has_value()) {
    return nullptr;
  }

  auto result = scope_pending[scope_id]->try_recv();
  if (std::holds_alternative<zenoh::channels::RecvError>(result)) {
    if (std::get<zenoh::channels::RecvError>(result) ==
        zenoh::channels::RecvError::Z_NODATA) {
      return nullptr;  // Still waiting, no reply (or timeout) yet.
    }
    // Z_DISCONNECTED: the query's timeout elapsed (or the producer vanished)
    // without ever replying. Clear the pending slot so the next
    // `request_data()` call is free to fire a new request for this scope --
    // the same silent, retry-next-tick drop a NNG `RecvTimeout` expiry got.
    scope_pending[scope_id].reset();
    return nullptr;
  }

  // A reply arrived: this request is no longer outstanding, whether or not
  // the reply itself carried an error.
  zenoh::Reply& reply = std::get<zenoh::Reply>(result);
  bool ok = reply.is_ok();
  std::vector<uint8_t> bytes;
  if (ok) {
    bytes = reply.get_ok().get_payload().as_vector();
  }
  scope_pending[scope_id].reset();

  if (!ok) {
    spdlog::error("ImscopeConsumer: scope {} query reply carried an error",
                  scope_id);
    return nullptr;
  }

  version++;
  return make_owned_msg_ptr(std::move(bytes));
}

ImscopeConsumer* ImscopeConsumer::connect(const char* name, const char* locator) {
  std::optional<zenoh::Session> session;
  try {
    session.emplace(open_zenoh_session(locator));
  } catch (const zenoh::ZException& e) {
    spdlog::error(
        "ImscopeConsumer: failed to open Zenoh session (locator='{}'): {}",
        locator ? locator : "", e.what());
    return nullptr;
  }

  std::string announce_key = std::string("imscope/") + name + "/announce";
  uint32_t magic = ANNOUNCE_MSG_ID;
  std::vector<uint8_t> req_bytes(sizeof(magic));
  std::memcpy(req_bytes.data(), &magic, sizeof(magic));

  zenoh::Session::GetOptions options = zenoh::Session::GetOptions::create_default();
  options.payload = zenoh::Bytes(std::move(req_bytes));
  options.timeout_ms = 2000;  // milliseconds, matches the old NNG RecvTimeo.

  std::vector<uint8_t> response_bytes;
  try {
    auto replies =
        session->get(announce_key, "", zenoh::channels::FifoChannel(1), std::move(options));
    auto res = replies.recv();
    if (!std::holds_alternative<zenoh::Reply>(res)) {
      spdlog::error("ImscopeConsumer: no announce reply from producer '{}'", name);
      return nullptr;
    }
    zenoh::Reply& reply = std::get<zenoh::Reply>(res);
    if (!reply.is_ok()) {
      spdlog::error("ImscopeConsumer: announce reply from '{}' carried an error",
                    name);
      return nullptr;
    }
    response_bytes = reply.get_ok().get_payload().as_vector();
  } catch (const zenoh::ZException& e) {
    spdlog::error("ImscopeConsumer: announce query to '{}' failed: {}", name,
                  e.what());
    return nullptr;
  }

  if (response_bytes.size() < sizeof(announce_response_t) - sizeof(imscope_scope_config_t)) {
    spdlog::error("ImscopeConsumer: announce reply from '{}' was too small", name);
    return nullptr;
  }

  announce_response_t* response =
      reinterpret_cast<announce_response_t*>(response_bytes.data());
  print_announce_response(response);
  auto consumer = new ImscopeConsumer(std::move(*session), response->num_scopes,
                                      response->scopes, response->name);
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

bool ImscopeConsumer::refresh_scopes() {
  if (!session.has_value()) {
    return false;
  }

  std::string announce_key = "imscope/" + name + "/announce";
  uint32_t magic = ANNOUNCE_MSG_ID;
  std::vector<uint8_t> req_bytes(sizeof(magic));
  std::memcpy(req_bytes.data(), &magic, sizeof(magic));

  zenoh::Session::GetOptions options = zenoh::Session::GetOptions::create_default();
  options.payload = zenoh::Bytes(std::move(req_bytes));
  options.timeout_ms = 200;  // fast enough to avoid UI lag, matches old value

  std::vector<uint8_t> response_bytes;
  try {
    auto replies =
        session->get(announce_key, "", zenoh::channels::FifoChannel(1), std::move(options));
    auto res = replies.recv();
    if (!std::holds_alternative<zenoh::Reply>(res)) {
      return false;
    }
    zenoh::Reply& reply = std::get<zenoh::Reply>(res);
    if (!reply.is_ok()) {
      return false;
    }
    response_bytes = reply.get_ok().get_payload().as_vector();
  } catch (const zenoh::ZException&) {
    return false;
  }

  if (response_bytes.size() < sizeof(announce_response_t) - sizeof(imscope_scope_config_t)) {
    return false;
  }
  announce_response_t* response =
      reinterpret_cast<announce_response_t*>(response_bytes.data());

  {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    size_t old_size = configured_scopes.size();
    if (response->num_scopes > (int)old_size) {
      for (int i = old_size; i < response->num_scopes; i++) {
        configured_scopes.push_back(response->scopes[i]);
        scope_pending.emplace_back();  // no request pending yet for the new scope
      }
    }
  }

  return true;
}

bool ImscopeConsumer::try_collect_int32(int scope_id,
                                        std::vector<int32_t>& values) {
  int version = 0;
  auto msg_ptr = try_collect_scope_msg(scope_id, version);
  if (!msg_ptr) {
    return false;
  }

  scope_msg_t* msg = static_cast<scope_msg_t*>(msg_ptr.get());
  size_t num_samples = msg->data_size / 4;
  int32_t* data = reinterpret_cast<int32_t*>(msg + 1);

  values.clear();
  values.reserve(num_samples);
  for (size_t i = 0; i < num_samples; ++i) {
    values.push_back(data[i]);
  }

  return true;
}

bool ImscopeConsumer::try_collect_float(int scope_id,
                                        std::vector<float>& values) {
  int version = 0;
  auto msg_ptr = try_collect_scope_msg(scope_id, version);
  if (!msg_ptr) {
    return false;
  }

  scope_msg_t* msg = static_cast<scope_msg_t*>(msg_ptr.get());
  size_t num_samples = msg->data_size / 4;
  float* data = reinterpret_cast<float*>(msg + 1);

  values.clear();
  values.reserve(num_samples);
  for (size_t i = 0; i < num_samples; ++i) {
    values.push_back(data[i]);
  }

  return true;
}
