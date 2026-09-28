/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#pragma once

#include <zenoh.hxx>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include "imscope_common.h"

class ImscopeConsumer;

// A collected scope message's owned byte buffer, type-erased behind
// `shared_ptr<void>` so callers can hold onto it (and the `scope_msg_t*`
// they cast it to, via `msg_ptr.get()`) for as long as they need. No longer
// backed by an `nng_msg` (the pointee is now a heap-allocated
// `std::vector<uint8_t>` produced from a Zenoh reply payload, kept alive via
// the shared_ptr's aliasing constructor) -- the type is kept named
// `NngMsgPtr` for source compatibility with existing call sites.
using NngMsgPtr = std::shared_ptr<void>;

class ImscopeConsumer {
  std::string name;
  std::optional<zenoh::Session> session;

  std::vector<imscope_scope_config_t> configured_scopes;
  // One slot per configured scope: holds the in-flight `get()` reply
  // channel for a scope while a `request_data()` call is outstanding, the
  // direct analog of the NNG version's per-scope `nng_ctx`/`nng_aio` pair
  // waiting on `nng_ctx_recv`. Empty when no request is outstanding.
  std::vector<std::optional<zenoh::channels::FifoHandler<zenoh::Reply>>>
      scope_pending;
  mutable std::mutex scopes_mutex;

 public:
  ImscopeConsumer(zenoh::Session&& session, int num_scopes,
                  imscope_scope_config_t* scopes, const char* name);
  ~ImscopeConsumer();

  imscope_return_t request_data(int scope_id);
  NngMsgPtr try_collect_scope_msg(int scope_id, int& handle);

  bool try_collect_iq(int scope_id, std::vector<int16_t>& real,
                      std::vector<int16_t>& imag);
  bool try_collect_real(int scope_id, std::vector<int16_t>& real);
  bool try_collect_int32(int scope_id, std::vector<int32_t>& values);
  bool try_collect_float(int scope_id, std::vector<float>& values);

  // `name` is the producer's name (the `imscope/<name>/...` key-expression
  // prefix); `locator` is an optional Zenoh endpoint to explicitly connect
  // to (e.g. "tcp/127.0.0.1:7447"). A null or empty `locator` relies on
  // Zenoh's default scouting/discovery instead.
  static ImscopeConsumer* connect(const char* name, const char* locator = nullptr);
  bool refresh_scopes();

  const std::string& get_name() const { return name; }

  const char* get_scope_name(int scope_id) const {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    return configured_scopes[scope_id].name;
  }

  int get_num_scopes() const {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    return configured_scopes.size();
  }
};
