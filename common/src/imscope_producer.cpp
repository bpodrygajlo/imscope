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
#include "imscope_discovery.h"
#include "imscope_internal.h"

#include <spdlog/spdlog.h>
#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <queue>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

#include "imscope_producer.h"

static std::string derive_control_address(const std::string& announce_addr) {
  if (announce_addr.rfind("tcp://", 0) == 0) {
    size_t colon = announce_addr.rfind(':');
    if (colon != std::string::npos) {
      std::string port_str = announce_addr.substr(colon + 1);
      try {
        int port = std::stoi(port_str);
        return announce_addr.substr(0, colon + 1) + std::to_string(port + 1);
      } catch (...) {
        // Fallback
      }
    }
  }
  return announce_addr + "-control";
}

// Settings are serialized on the wire in a fixed 64-byte name field
// (imscope_setting_t::name / setting_request_t::name). Truncate to that
// width at registration so a name comparison against a wire-truncated
// request can never match the wrong setting.
static std::string truncate_setting_name(const char* name) {
  std::string s(name);
  constexpr size_t kMaxLen = sizeof(((imscope_setting_t*)nullptr)->name) - 1;
  if (s.size() > kMaxLen) {
    s.resize(kMaxLen);
  }
  return s;
}

struct SettingInfo {
  std::string name;
  setting_type_t type;

  union {
    bool bval;
    int32_t ival;
    float fval;
  } value;

  imscope_setting_bool_cb_t bool_cb = nullptr;
  imscope_setting_int32_cb_t int32_cb = nullptr;
  imscope_setting_float_cb_t float_cb = nullptr;
};

typedef struct {
  std::string name;
  std::string group;
  scope_type_t type;
  scope_domain_t domain = SCOPE_DOMAIN_TIME;
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
            self->parent->active_requests[self->id].push_back(self);
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
            std::lock_guard<std::mutex> lock(self->parent->scopes_mutex);
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
            strncpy(msg->control_address, self->parent->control_address.c_str(),
                    sizeof(msg->control_address) - 1);
            strncpy(msg->name, self->parent->name.c_str(),
                    sizeof(msg->name) - 1);
            for (size_t i = 0; i < self->parent->configured_scopes.size();
                 i++) {
              strncpy(msg->scopes[i].name,
                      self->parent->configured_scopes[i].name.c_str(),
                      MAX_SCOPE_NAME_LEN - 1);
              strncpy(msg->scopes[i].group,
                      self->parent->configured_scopes[i].group.c_str(),
                      MAX_GROUP_NAME_LEN - 1);
              msg->scopes[i].type = self->parent->configured_scopes[i].type;
              msg->scopes[i].domain = self->parent->configured_scopes[i].domain;
            }

            spdlog::debug(
                "ImscopeProducer: Announced {} scopes to consumer."
                "Data address: {} Announce address: {} Control address: {}",
                self->parent->configured_scopes.size(),
                self->parent->data_address, self->parent->announce_address,
                self->parent->control_address);

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

  struct SettingsCtx {
    nng_ctx ctx;
    nng_aio* send_aio;
    nng_aio* recv_aio;
    ImscopeProducer* parent;

    SettingsCtx(nng_socket socket, ImscopeProducer* p) : parent(p) {
      nng_ctx_open(&ctx, socket);
      nng_aio_alloc(&send_aio, NULL, NULL);
      nng_aio_alloc(&recv_aio, settings_callback, this);
      nng_ctx_recv(ctx, recv_aio);
    }

    ~SettingsCtx() {
      nng_aio_free(send_aio);
      nng_aio_free(recv_aio);
      nng_ctx_close(ctx);
    }

    static void settings_callback(void* arg) {
      auto self = static_cast<SettingsCtx*>(arg);
      int rv = nng_aio_result(self->recv_aio);
      if (rv == 0) {
        nng_msg* req_msg = nng_aio_get_msg(self->recv_aio);
        if (nng_msg_len(req_msg) >= sizeof(setting_request_t)) {
          setting_request_t* req = (setting_request_t*)nng_msg_body(req_msg);
          if (req->magic == SETTING_REQ_GET_ALL) {
            std::lock_guard<std::mutex> lock(self->parent->settings_mutex);
            size_t num_settings = self->parent->registered_settings.size();
            size_t size = sizeof(setting_response_t);
            if (num_settings > 1) {
              size += (num_settings - 1) * sizeof(imscope_setting_t);
            } else if (num_settings == 0) {
              size -= sizeof(imscope_setting_t);
            }
            nng_msg* res_msg;
            nng_msg_alloc(&res_msg, size);
            setting_response_t* res =
                (setting_response_t*)nng_msg_body(res_msg);
            res->magic = SETTING_REP_GET_ALL;
            res->status = 0;
            res->num_settings = num_settings;
            for (size_t i = 0; i < num_settings; i++) {
              const auto& s = self->parent->registered_settings[i];
              strncpy(res->settings[i].name, s.name.c_str(),
                      sizeof(res->settings[i].name) - 1);
              res->settings[i].name[sizeof(res->settings[i].name) - 1] = '\0';
              res->settings[i].type = s.type;
              if (s.type == SETTING_TYPE_BOOL) {
                res->settings[i].value.bval = s.value.bval ? 1 : 0;
              } else if (s.type == SETTING_TYPE_INT32) {
                res->settings[i].value.ival = s.value.ival;
              } else if (s.type == SETTING_TYPE_FLOAT) {
                res->settings[i].value.fval = s.value.fval;
              }
            }
            nng_aio_set_msg(self->send_aio, res_msg);
            nng_ctx_send(self->ctx, self->send_aio);
          } else if (req->magic == SETTING_REQ_SET) {
            std::lock_guard<std::mutex> lock(self->parent->settings_mutex);
            bool found = false;
            int status = -1;
            for (auto& s : self->parent->registered_settings) {
              if (strncmp(s.name.c_str(), req->name, sizeof(req->name)) == 0) {
                found = true;
                if (s.type == req->type) {
                  if (s.type == SETTING_TYPE_BOOL) {
                    s.value.bval = req->value.bval != 0;
                    if (s.bool_cb)
                      s.bool_cb(s.value.bval);
                  } else if (s.type == SETTING_TYPE_INT32) {
                    s.value.ival = req->value.ival;
                    if (s.int32_cb)
                      s.int32_cb(s.value.ival);
                  } else if (s.type == SETTING_TYPE_FLOAT) {
                    s.value.fval = req->value.fval;
                    if (s.float_cb)
                      s.float_cb(s.value.fval);
                  }
                  status = 0;
                } else {
                  status = -2;  // type mismatch
                }
                break;
              }
            }
            if (!found) {
              status = -3;  // not found
            }
            nng_msg* res_msg;
            nng_msg_alloc(&res_msg, sizeof(setting_response_t) -
                                        sizeof(imscope_setting_t));
            setting_response_t* res =
                (setting_response_t*)nng_msg_body(res_msg);
            res->magic = SETTING_REP_SET;
            res->status = status;
            res->num_settings = 0;
            nng_aio_set_msg(self->send_aio, res_msg);
            nng_ctx_send(self->ctx, self->send_aio);
          }
        }
        nng_msg_free(req_msg);
        nng_ctx_recv(self->ctx, self->recv_aio);
      } else if (rv != (int)NNG_ECLOSED) {
        nng_ctx_recv(self->ctx, self->recv_aio);
      }
    }
  };

  std::vector<std::unique_ptr<ScopeCtx>> workers;
  std::unique_ptr<AnnounceCtx> announce_handler;
  std::unique_ptr<SettingsCtx> settings_handler;
  std::unordered_map<int, std::vector<ScopeCtx*>> active_requests;
  std::mutex active_requests_mutex;
  std::mutex scopes_mutex;
  std::mutex acquired_msgs_mutex;

  std::mutex settings_mutex;
  std::vector<SettingInfo> registered_settings;
  std::string control_address;
  nng_socket control_socket = NNG_SOCKET_INITIALIZER;
  void* discovery_server_handle = nullptr;

  std::mutex scalar_mutex;
  std::thread flush_thread;
  std::atomic<bool> flush_thread_running{false};

  struct ScalarAccumulator {
    std::vector<uint32_t> buffer;
    std::chrono::steady_clock::time_point last_send_time;

    ScalarAccumulator() { last_send_time = std::chrono::steady_clock::now(); }
  };

  std::unordered_map<int, ScalarAccumulator> scalar_accumulators;

 public:
  // Sends the accumulated scalar buffer for `id` and clears it. Caller must
  // already hold scalar_mutex. Returns IMSCOPE_SUCCESS even if no worker is
  // currently waiting on this scope (the buffer stays queued for later).
  imscope_return_t send_accumulator_locked(
      int id, ScalarAccumulator& acc,
      std::chrono::steady_clock::time_point now) {
    std::vector<ScopeCtx*> waiting;
    {
      std::lock_guard<std::mutex> active_lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it != active_requests.end()) {
        waiting = std::move(it->second);
        active_requests.erase(it);
      }
    }
    if (waiting.empty()) {
      return IMSCOPE_SUCCESS;
    }

    auto start = std::chrono::high_resolution_clock::now();
    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * acc.buffer.size();
    nng_msg* base_msg = nullptr;
    if (nng_msg_alloc(&base_msg, size) != 0) {
      for (auto* worker : waiting) {
        worker->req_received.store(false);
        nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
        if (req_msg)
          nng_msg_free(req_msg);
        nng_ctx_recv(worker->ctx, worker->recv_aio);
      }
      return IMSCOPE_ERROR_INTERNAL;
    }

    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(base_msg);
    msg->meta = {};
    msg->id = id;
    msg->data_size = acc.buffer.size() * sizeof(uint32_t);
    memcpy((void*)(msg + 1), acc.buffer.data(),
           sizeof(uint32_t) * acc.buffer.size());
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    for (size_t i = 0; i < waiting.size(); i++) {
      ScopeCtx* worker = waiting[i];
      worker->req_received.store(false);
      nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
      if (req_msg) {
        nng_msg_free(req_msg);
      }

      nng_msg* msg_to_send = nullptr;
      if (i == waiting.size() - 1) {
        msg_to_send = base_msg;
      } else {
        if (nng_msg_dup(&msg_to_send, base_msg) != 0) {
          continue;
        }
      }

      nng_aio_set_msg(worker->send_aio, msg_to_send);
      nng_ctx_send(worker->ctx, worker->send_aio);
    }

    int last_rv = 0;
    for (auto* worker : waiting) {
      nng_aio_wait(worker->send_aio);
      int rv = nng_aio_result(worker->send_aio);
      if (rv != 0) {
        last_rv = rv;
      }
      nng_ctx_recv(worker->ctx, worker->recv_aio);
    }

    acc.buffer.clear();
    acc.last_send_time = now;

    return last_rv == 0 ? IMSCOPE_SUCCESS : IMSCOPE_ERROR_INTERNAL;
  }

  imscope_return_t push_scalar_value(uint32_t val, int id) {
    std::lock_guard<std::mutex> lock(scalar_mutex);
    auto& acc = scalar_accumulators[id];
    acc.buffer.push_back(val);

    if (acc.buffer.size() > 10000) {
      acc.buffer.erase(acc.buffer.begin(),
                       acc.buffer.begin() + (acc.buffer.size() - 10000));
    }

    bool threshold_reached = acc.buffer.size() >= 256;
    auto now = std::chrono::steady_clock::now();
    auto elapsed_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                          now - acc.last_send_time)
                          .count();
    bool timeout_reached = elapsed_ms >= 30 && !acc.buffer.empty();

    if (threshold_reached || timeout_reached) {
      return send_accumulator_locked(id, acc, now);
    }
    return IMSCOPE_SUCCESS;
  }

  void flush_all_accumulators() {
    std::lock_guard<std::mutex> lock(scalar_mutex);
    auto now = std::chrono::steady_clock::now();
    for (auto& pair : scalar_accumulators) {
      int id = pair.first;
      auto& acc = pair.second;
      if (acc.buffer.empty()) {
        continue;
      }
      auto elapsed_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                            now - acc.last_send_time)
                            .count();
      if (elapsed_ms >= 30) {
        send_accumulator_locked(id, acc, now);
      }
    }
  }

  void start_flush_thread() {
    flush_thread_running = true;
    flush_thread = std::thread([this]() {
      while (flush_thread_running) {
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
        flush_all_accumulators();
      }
    });
  }

  void stop_flush_thread() {
    flush_thread_running = false;
    if (flush_thread.joinable()) {
      flush_thread.join();
    }
  }

 public:
  imscope_return_t register_setting_bool(const char* name, bool initial_val,
                                         imscope_setting_bool_cb_t callback) {
    std::string truncated_name = truncate_setting_name(name);
    std::lock_guard<std::mutex> lock(settings_mutex);
    for (auto& s : registered_settings) {
      if (s.name == truncated_name) {
        s.type = SETTING_TYPE_BOOL;
        s.value.bval = initial_val;
        s.bool_cb = callback;
        return IMSCOPE_SUCCESS;
      }
    }
    SettingInfo s;
    s.name = truncated_name;
    s.type = SETTING_TYPE_BOOL;
    s.value.bval = initial_val;
    s.bool_cb = callback;
    registered_settings.push_back(s);
    return IMSCOPE_SUCCESS;
  }

  imscope_return_t register_setting_int32(const char* name, int32_t initial_val,
                                          imscope_setting_int32_cb_t callback) {
    std::string truncated_name = truncate_setting_name(name);
    std::lock_guard<std::mutex> lock(settings_mutex);
    for (auto& s : registered_settings) {
      if (s.name == truncated_name) {
        s.type = SETTING_TYPE_INT32;
        s.value.ival = initial_val;
        s.int32_cb = callback;
        return IMSCOPE_SUCCESS;
      }
    }
    SettingInfo s;
    s.name = truncated_name;
    s.type = SETTING_TYPE_INT32;
    s.value.ival = initial_val;
    s.int32_cb = callback;
    registered_settings.push_back(s);
    return IMSCOPE_SUCCESS;
  }

  imscope_return_t register_setting_float(const char* name, float initial_val,
                                          imscope_setting_float_cb_t callback) {
    std::string truncated_name = truncate_setting_name(name);
    std::lock_guard<std::mutex> lock(settings_mutex);
    for (auto& s : registered_settings) {
      if (s.name == truncated_name) {
        s.type = SETTING_TYPE_FLOAT;
        s.value.fval = initial_val;
        s.float_cb = callback;
        return IMSCOPE_SUCCESS;
      }
    }
    SettingInfo s;
    s.name = truncated_name;
    s.type = SETTING_TYPE_FLOAT;
    s.value.fval = initial_val;
    s.float_cb = callback;
    registered_settings.push_back(s);
    return IMSCOPE_SUCCESS;
  }

  static int bind_socket_url(nng_socket socket, const std::string& address,
                             std::string& out_advertised_url) {
    std::string bind_address = address;
    if (bind_address.empty()) {
      bind_address = "tcp://0.0.0.0:0";
    }

    nng_listener listener;
    int rv = nng_listener_create(&listener, socket, bind_address.c_str());
    if (rv != 0) {
      return rv;
    }
    rv = nng_listener_start(listener, 0);
    if (rv != 0) {
      nng_listener_close(listener);
      return rv;
    }

    int port = 0;
    if (nng_listener_get_int(listener, NNG_OPT_TCP_BOUND_PORT, &port) == 0 &&
        port > 0) {
      std::string ip = "127.0.0.1";
      if (bind_address.find("0.0.0.0") != std::string::npos) {
        ip = imscope_get_primary_ip();
      } else {
        size_t prefix = bind_address.find("://");
        if (prefix != std::string::npos) {
          size_t start = prefix + 3;
          size_t colon = bind_address.rfind(':');
          if (colon != std::string::npos && colon > start) {
            ip = bind_address.substr(start, colon - start);
          }
        }
      }
      out_advertised_url = "tcp://" + ip + ":" + std::to_string(port);
    } else {
      out_advertised_url = bind_address;
    }
    return 0;
  }

  ImscopeProducer() {}

  ~ImscopeProducer() {
    stop_flush_thread();
    if (discovery_server_handle) {
      imscope_discovery_server_stop(discovery_server_handle);
      discovery_server_handle = nullptr;
    }
    announce_handler.reset();
    settings_handler.reset();
    if (nng_socket_id(announce_socket) > 0) {
      nng_close(announce_socket);
    }
    if (nng_socket_id(control_socket) > 0) {
      nng_close(control_socket);
    }
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      active_requests.clear();
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

  void clear_scopes() {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    configured_scopes.clear();
    {
      std::lock_guard<std::mutex> s_lock(scalar_mutex);
      scalar_accumulators.clear();
    }
  }

  void connect(const char* data_address, const char* announce_address,
               const char* name) {
    stop_flush_thread();

    if (discovery_server_handle) {
      imscope_discovery_server_stop(discovery_server_handle);
      discovery_server_handle = nullptr;
    }

    this->name = (name && strlen(name) > 0) ? name : "imscope";

    std::string req_data_addr = (data_address && strlen(data_address) > 0)
                                    ? data_address
                                    : "";
    std::string req_ann_addr =
        (announce_address && strlen(announce_address) > 0) ? announce_address
                                                           : "";

    // If announce_address was passed as just a name (not URL), use it as name
    if (!req_ann_addr.empty() &&
        req_ann_addr.rfind("tcp://", 0) != 0 &&
        req_ann_addr.rfind("ipc://", 0) != 0 &&
        req_ann_addr.rfind("inproc://", 0) != 0) {
      this->name = req_ann_addr;
      req_ann_addr = "";
    }

    // Default to dynamic TCP if not specified
    if (req_ann_addr.empty()) {
      req_ann_addr = "tcp://0.0.0.0:0";
    }
    if (req_data_addr.empty()) {
      if (req_ann_addr.rfind("inproc://", 0) == 0) {
        req_data_addr = req_ann_addr + "-data";
      } else if (req_ann_addr.rfind("ipc://", 0) == 0) {
        req_data_addr = req_ann_addr + "-data";
      } else {
        req_data_addr = "tcp://0.0.0.0:0";
      }
    }

    // Clear previous state if any
    workers.clear();
    active_requests.clear();
    acquired_msgs.clear();
    {
      std::lock_guard<std::mutex> s_lock(scalar_mutex);
      scalar_accumulators.clear();
    }
    if (nng_socket_id(data_socket) > 0) {
      nng_close(data_socket);
    }
    if (nng_socket_id(announce_socket) > 0) {
      nng_close(announce_socket);
    }
    if (nng_socket_id(control_socket) > 0) {
      nng_close(control_socket);
    }

    // Open and bind data socket
    nng_rep0_open(&data_socket);
    int rv = bind_socket_url(data_socket, req_data_addr, this->data_address);
    if (rv != 0) {
      spdlog::error("ImscopeProducer: Failed to bind data socket to {}: {}",
                    req_data_addr, nng_strerror(rv));
      return;
    }

    // Open and bind announce socket
    nng_rep0_open(&announce_socket);
    rv = bind_socket_url(announce_socket, req_ann_addr, this->announce_address);
    if (rv != 0) {
      spdlog::error("ImscopeProducer: Failed to bind announce socket to {}: {}",
                    req_ann_addr, nng_strerror(rv));
      return;
    }

    // Derive or bind control socket
    std::string req_ctl_addr;
    if (req_ann_addr.rfind("tcp://", 0) == 0) {
      req_ctl_addr = "tcp://0.0.0.0:0";
    } else {
      req_ctl_addr = derive_control_address(this->announce_address);
    }

    nng_rep0_open(&control_socket);
    rv = bind_socket_url(control_socket, req_ctl_addr, this->control_address);
    if (rv != 0) {
      spdlog::error("ImscopeProducer: Failed to bind control socket to {}: {}",
                    req_ctl_addr, nng_strerror(rv));
    } else {
      settings_handler =
          std::make_unique<SettingsCtx>(this->control_socket, this);
    }

    announce_handler =
        std::make_unique<AnnounceCtx>(this->announce_socket, this);

    std::lock_guard<std::mutex> lock(scopes_mutex);
    size_t num_workers = std::max((size_t)64, configured_scopes.size() * 4);
    for (size_t i = 0; i < num_workers; i++) {
      workers.push_back(std::make_unique<ScopeCtx>(this->data_socket, this));
    }

    spdlog::info(
        "ImscopeProducer '{}' started. Announce: {}, Data: {}, Control: {}",
        this->name, this->announce_address, this->data_address,
        this->control_address);

    if (this->announce_address.rfind("tcp://", 0) == 0) {
      discovery_server_handle =
          imscope_discovery_server_start(this->name, this->announce_address);
    }

    start_flush_thread();
  }

  imscope_return_t add_scope(const char* name, scope_type_t type,
                             const char* group = "",
                             scope_domain_t domain = SCOPE_DOMAIN_TIME) {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    for (size_t i = 0; i < configured_scopes.size(); i++) {
      if (configured_scopes[i].name == name) {
        return IMSCOPE_SUCCESS;
      }
    }
    scope_info_t scope_info = {name, group ? group : "", type, domain};
    configured_scopes.push_back(scope_info);
    if (nng_socket_id(data_socket) > 0) {
      for (int i = 0; i < 4; i++) {
        workers.push_back(std::make_unique<ScopeCtx>(this->data_socket, this));
      }
    }
    return IMSCOPE_SUCCESS;
  }

  int get_or_register_scope(const char* name, scope_type_t type,
                             const char* group = "",
                             scope_domain_t domain = SCOPE_DOMAIN_TIME) {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    for (size_t i = 0; i < configured_scopes.size(); i++) {
      if (configured_scopes[i].name == name) {
        return static_cast<int>(i);
      }
    }
    scope_info_t scope_info = {name, group ? group : "", type, domain};
    configured_scopes.push_back(scope_info);
    if (nng_socket_id(data_socket) > 0) {
      for (int i = 0; i < 4; i++) {
        workers.push_back(std::make_unique<ScopeCtx>(this->data_socket, this));
      }
    }
    return static_cast<int>(configured_scopes.size() - 1);
  }

  int get_scope_id_by_name(const char* name) {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    for (size_t i = 0; i < configured_scopes.size(); i++) {
      if (configured_scopes[i].name == name) {
        return static_cast<int>(i);
      }
    }
    return -1;
  }

  imscope_return_t send_scope_data(uint32_t* data, int id, size_t num_samples,
                                   int frame, int slot, uint64_t timestamp) {
    std::vector<ScopeCtx*> waiting;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end() || it->second.empty()) {
        return IMSCOPE_ERROR_BUSY;
      }
      waiting = std::move(it->second);
      active_requests.erase(it);
    }

    auto start = std::chrono::high_resolution_clock::now();

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    nng_msg* base_msg = nullptr;
    if (nng_msg_alloc(&base_msg, size) != 0) {
      for (auto* worker : waiting) {
        worker->req_received.store(false);
        nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
        if (req_msg)
          nng_msg_free(req_msg);
        nng_ctx_recv(worker->ctx, worker->recv_aio);
      }
      return IMSCOPE_ERROR_INTERNAL;
    }

    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(base_msg);
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

    for (size_t i = 0; i < waiting.size(); i++) {
      ScopeCtx* worker = waiting[i];
      worker->req_received.store(false);
      nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
      if (req_msg) {
        nng_msg_free(req_msg);
      }

      nng_msg* msg_to_send = nullptr;
      if (i == waiting.size() - 1) {
        msg_to_send = base_msg;
      } else {
        if (nng_msg_dup(&msg_to_send, base_msg) != 0) {
          continue;
        }
      }

      nng_aio_set_msg(worker->send_aio, msg_to_send);
      nng_ctx_send(worker->ctx, worker->send_aio);
    }

    int last_rv = 0;
    for (auto* worker : waiting) {
      nng_aio_wait(worker->send_aio);
      int rv = nng_aio_result(worker->send_aio);
      if (rv != 0) {
        last_rv = rv;
      }
      nng_ctx_recv(worker->ctx, worker->recv_aio);
    }

    return last_rv == 0 ? IMSCOPE_SUCCESS : IMSCOPE_ERROR_INTERNAL;
  }

  std::unordered_map<int, nng_msg*> acquired_msgs;

  void* acquire_buffer(int id, size_t num_samples) {
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end() || it->second.empty()) {
        return nullptr;
      }
    }

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    nng_msg* allocated_msg = nullptr;
    if (nng_msg_alloc(&allocated_msg, size) != 0) {
      return nullptr;
    }

    {
      std::lock_guard<std::mutex> lock(acquired_msgs_mutex);
      acquired_msgs[id] = allocated_msg;
    }

    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(allocated_msg);
    msg->id = id;
    return (void*)(msg + 1);
  }

  imscope_return_t commit_buffer(int id, size_t num_samples, int frame,
                                 int slot, uint64_t timestamp) {
    nng_msg* base_msg = nullptr;
    {
      std::lock_guard<std::mutex> lock(acquired_msgs_mutex);
      auto it = acquired_msgs.find(id);
      if (it == acquired_msgs.end() || it->second == nullptr) {
        return IMSCOPE_ERROR_NOT_INITIALIZED;
      }
      base_msg = it->second;
      acquired_msgs.erase(it);
    }

    std::vector<ScopeCtx*> waiting;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it != active_requests.end()) {
        waiting = std::move(it->second);
        active_requests.erase(it);
      }
    }

    if (waiting.empty()) {
      nng_msg_free(base_msg);
      return IMSCOPE_ERROR_BUSY;
    }

    auto start = std::chrono::high_resolution_clock::now();
    scope_msg_t* msg = (scope_msg_t*)nng_msg_body(base_msg);
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    for (size_t i = 0; i < waiting.size(); i++) {
      ScopeCtx* worker = waiting[i];
      worker->req_received.store(false);
      nng_msg* req_msg = nng_aio_get_msg(worker->recv_aio);
      if (req_msg) {
        nng_msg_free(req_msg);
      }

      nng_msg* msg_to_send = nullptr;
      if (i == waiting.size() - 1) {
        msg_to_send = base_msg;
      } else {
        if (nng_msg_dup(&msg_to_send, base_msg) != 0) {
          continue;
        }
      }

      nng_aio_set_msg(worker->send_aio, msg_to_send);
      nng_ctx_send(worker->ctx, worker->send_aio);
    }

    int last_rv = 0;
    for (auto* worker : waiting) {
      nng_aio_wait(worker->send_aio);
      int rv = nng_aio_result(worker->send_aio);
      if (rv != 0) {
        last_rv = rv;
      }
      nng_ctx_recv(worker->ctx, worker->recv_aio);
    }

    return last_rv == 0 ? IMSCOPE_SUCCESS : IMSCOPE_ERROR_INTERNAL;
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
  if (scopes != nullptr) {
    for (size_t i = 0; i < num_scopes; ++i) {
      instance->add_scope(scopes[i].name, scopes[i].type, "", scopes[i].domain);
    }
  }
  instance->connect(data_address, announce_address, name);
  return IMSCOPE_SUCCESS;
}

extern "C" int imscope_register_scope(const char* name, scope_type_t type) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->get_or_register_scope(name, type);
}

extern "C" int imscope_register_scope_with_domain(const char* name,
                                                  scope_type_t type,
                                                  scope_domain_t domain) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->get_or_register_scope(name, type, "", domain);
}

extern "C" imscope_return_t imscope_try_send_data_by_name(
    uint32_t* data, const char* name, scope_type_t type, size_t num_samples,
    int frame, int slot, uint64_t timestamp) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_or_register_scope(name, type);
  return instance->send_scope_data(data, id, num_samples, frame, slot,
                                   timestamp);
}

extern "C" void* imscope_acquire_send_buffer_by_name(const char* name,
                                                     scope_type_t type,
                                                     size_t num_samples) {
  if (instance == nullptr) {
    return nullptr;
  }
  int id = instance->get_or_register_scope(name, type);
  return instance->acquire_buffer(id, num_samples);
}

extern "C" imscope_return_t imscope_commit_send_buffer_by_name(
    const char* name, size_t num_samples, int frame, int slot,
    uint64_t timestamp) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_scope_id_by_name(name);
  if (id < 0) {
    return IMSCOPE_ERROR_INVALID_ID;
  }
  return instance->commit_buffer(id, num_samples, frame, slot, timestamp);
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

extern "C" imscope_return_t imscope_try_send_int32(int32_t val, int id) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_try_send_float(float val, int id) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_try_send_int32_by_name(int32_t val,
                                                           const char* name) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_or_register_scope(name, SCOPE_TYPE_INT32);
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_try_send_float_by_name(float val,
                                                           const char* name) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_or_register_scope(name, SCOPE_TYPE_FLOAT);
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_try_send_int32_by_group(int32_t val,
                                                            const char* name,
                                                            const char* group) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_or_register_scope(name, SCOPE_TYPE_INT32, group);
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_try_send_float_by_group(float val,
                                                            const char* name,
                                                            const char* group) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  int id = instance->get_or_register_scope(name, SCOPE_TYPE_FLOAT, group);
  uint32_t val_u;
  memcpy(&val_u, &val, sizeof(val_u));
  return instance->push_scalar_value(val_u, id);
}

extern "C" imscope_return_t imscope_register_setting_bool(
    const char* name, bool initial_val, imscope_setting_bool_cb_t callback) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->register_setting_bool(name, initial_val, callback);
}

extern "C" imscope_return_t imscope_register_setting_int32(
    const char* name, int32_t initial_val,
    imscope_setting_int32_cb_t callback) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->register_setting_int32(name, initial_val, callback);
}

extern "C" imscope_return_t imscope_register_setting_float(
    const char* name, float initial_val, imscope_setting_float_cb_t callback) {
  if (instance == nullptr) {
    return IMSCOPE_ERROR_NOT_INITIALIZED;
  }
  return instance->register_setting_float(name, initial_val, callback);
}
