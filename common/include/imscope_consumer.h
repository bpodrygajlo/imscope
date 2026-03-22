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

class ContextWorker;
using NngMsgPtr = std::shared_ptr<void>;

NngMsgPtr make_nng_msg_ptr(nng_msg* msg, ContextWorker* worker);

class ImscopeConsumer {
  friend class ContextWorker;

  struct ScopeBuffer {
    NngMsgPtr msg;
    std::mutex mutex;
    int version = 0;
    ScopeBuffer() = default;
    ScopeBuffer(const ScopeBuffer&) = delete;
    ScopeBuffer& operator=(const ScopeBuffer&) = delete;
  };

  std::string data_address;
  std::string announce_address;
  nng_socket data_socket;
  std::vector<std::unique_ptr<ScopeBuffer>> scope_buffers;

  std::vector<imscope_scope_config_t> configured_scopes;
  std::vector<std::unique_ptr<ContextWorker>> workers;
  std::string name;

 public:
  ImscopeConsumer(const char* data_address, const char* announce_address,
                  int num_scopes, imscope_scope_config_t* scopes,
                  const char* name);
  ~ImscopeConsumer();
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

  static void free(scope_msg_t* msg);
};
