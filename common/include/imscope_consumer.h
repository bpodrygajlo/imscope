/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#pragma once

#include <nng/nng.h>
#include <nng/protocol/reqrep0/rep.h>
#include <spdlog/spdlog.h>
#include <memory>
#include <mutex>
#include <vector>

#include "imscope_common.h"

class ImscopeConsumer;
using NngMsgPtr = std::shared_ptr<void>;

NngMsgPtr make_nng_msg_ptr(nng_msg* msg);

class ImscopeConsumer {
  std::string data_address;
  std::string announce_address;
  nng_socket data_socket;

  struct ScopeCtx;
  std::vector<imscope_scope_config_t> configured_scopes;
  std::vector<std::unique_ptr<ScopeCtx>> scope_contexts;
  std::string name;
  mutable std::mutex scopes_mutex;

 public:
  ImscopeConsumer(const char* data_address, int num_scopes,
                  imscope_scope_config_t* scopes, const char* name);
  ~ImscopeConsumer();

  imscope_return_t request_data(int scope_id);
  NngMsgPtr try_collect_scope_msg(int scope_id, int& handle);

  bool try_collect_iq(int scope_id, std::vector<int16_t>& real,
                      std::vector<int16_t>& imag);
  bool try_collect_real(int scope_id, std::vector<int16_t>& real);
  bool try_collect_int32(int scope_id, std::vector<int32_t>& values);
  bool try_collect_float(int scope_id, std::vector<float>& values);
  static ImscopeConsumer* connect(const char* announce_address);
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
