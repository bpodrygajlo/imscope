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

 public:
  ImscopeConsumer(const char* data_address, int num_scopes,
                  imscope_scope_config_t* scopes, const char* name);
  ~ImscopeConsumer();

  imscope_return_t request_data(int scope_id);
  NngMsgPtr try_collect_scope_msg(int scope_id, int& handle);

  bool try_collect_iq(int scope_id, std::vector<int16_t>& real,
                      std::vector<int16_t>& imag);
  bool try_collect_real(int scope_id, std::vector<int16_t>& real);
  static ImscopeConsumer* connect(const char* announce_address);

  const std::string& get_name() const { return name; }

  const char* get_scope_name(int scope_id) const {
    return configured_scopes[scope_id].name;
  }

  int get_num_scopes() const { return configured_scopes.size(); }
};
