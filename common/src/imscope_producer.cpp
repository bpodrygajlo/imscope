/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <zenoh.hxx>
#include "imscope_common.h"
#include "imscope_internal.h"

#include <spdlog/spdlog.h>
#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

#include "imscope_producer.h"

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

// A producer using Zenoh instead of NNG. The general shape mirrors the
// NNG-based `ImscopeProducer` this replaces (and the Rust `Producer` in
// `producer/src/producer.rs`): one `zenoh::Session`, a `Queryable` each for
// the announce and control key expressions (which reply synchronously
// inside the callback), and one data `Queryable` per scope which instead
// clones and parks the `Query` in `active_requests` until a `try_send_*`
// call claims and replies to it. Compared to the old NNG worker-context
// pool (`ScopeCtx`/`AnnounceCtx`/`SettingsCtx`, each wrapping an `nng_ctx` +
// `nng_aio` pair that had to be manually re-armed with `nng_ctx_recv` after
// every request), a Zenoh `Queryable` callback simply fires again for every
// subsequent query with no re-arming needed, so the "worker pool" concept
// disappears entirely.
class ImscopeProducer {
  std::string name;
  std::string data_key_prefix;
  std::string control_key;

  std::optional<zenoh::Session> session;

  std::vector<scope_info_t> configured_scopes;
  std::mutex scopes_mutex;

  // Queries parked here are waiting for a `try_send_*`/`commit_buffer` call
  // to claim and reply to them -- the direct analog of the NNG version's
  // `active_requests` map of `ScopeCtx*`, and of the Rust producer's
  // `Mutex<HashMap<i32, PendingSlot>>` (`producer/src/producer.rs`).
  std::unordered_map<int, zenoh::Query> active_requests;
  std::mutex active_requests_mutex;

  // Buffers acquired via `acquire_buffer()` for zero-copy sends, keyed by
  // scope id, claimed and sent by a later `commit_buffer()` call.
  std::unordered_map<int, std::vector<uint8_t>> acquired_buffers;
  std::mutex acquired_buffers_mutex;

  std::mutex settings_mutex;
  std::vector<SettingInfo> registered_settings;

  std::mutex scalar_mutex;
  std::thread flush_thread;
  std::atomic<bool> flush_thread_running{false};

  struct ScalarAccumulator {
    std::vector<uint32_t> buffer;
    std::chrono::steady_clock::time_point last_send_time;

    ScalarAccumulator() { last_send_time = std::chrono::steady_clock::now(); }
  };

  std::unordered_map<int, ScalarAccumulator> scalar_accumulators;

  // Declares the data `Queryable` for `scope_id` (key expression
  // `imscope/<name>/data/<scope_id>`), if a session is currently open.
  // Called both for the scopes configured at `connect()` time and for
  // scopes registered dynamically afterwards (`add_scope`/
  // `get_or_register_scope`). A `declare_background_queryable` answers
  // until the session itself is closed/destroyed, so nothing needs to be
  // kept alive to hold it open -- unlike the NNG version's `workers` vector
  // of `ScopeCtx` objects.
  void declare_data_queryable(int scope_id) {
    if (!session.has_value()) {
      return;
    }
    std::string key = data_key_prefix + "/" + std::to_string(scope_id);
    try {
      session->declare_background_queryable(
          key, [this](zenoh::Query& query) { handle_data_query(query); },
          []() {});
    } catch (const zenoh::ZException& e) {
      spdlog::error(
          "ImscopeProducer: failed to declare data queryable for scope {} "
          "(key '{}'): {}",
          scope_id, key, e.what());
    }
  }

  // On a valid scope data request, clones the `Query` and parks it in
  // `active_requests` keyed by the requested scope id, instead of replying
  // immediately -- the reply is sent later by `send_scope_data()` /
  // `commit_buffer()` / `send_accumulator_locked()`, once the application
  // has actual sample data to hand back. If nothing ever sends for this
  // scope, the `Query` is simply dropped (replaced by the next request, or
  // dropped along with the producer) and the requester's `get()` times out
  // -- the same drop-if-nobody-produces semantics the NNG version had.
  void handle_data_query(zenoh::Query& query) {
    auto payload = query.get_payload();
    if (!payload.has_value()) {
      return;
    }
    std::vector<uint8_t> bytes = payload->get().as_vector();
    if (bytes.size() < sizeof(scope_request_t)) {
      return;
    }
    scope_request_t req;
    std::memcpy(&req, bytes.data(), sizeof(req));
    if (req.magic != SCOPE_REQ_MSG_ID) {
      return;
    }

    zenoh::Query cloned = query.clone();
    std::lock_guard<std::mutex> lock(active_requests_mutex);
    active_requests.insert_or_assign(req.scope_id, std::move(cloned));
  }

  // Replies synchronously to an announce request, mirroring the NNG
  // version's `AnnounceCtx::announce_callback`.
  void handle_announce_query(zenoh::Query& query) {
    auto payload = query.get_payload();
    if (!payload.has_value()) {
      return;
    }
    std::vector<uint8_t> req_bytes = payload->get().as_vector();
    if (req_bytes.size() < sizeof(announce_request_t)) {
      return;
    }
    announce_request_t req;
    std::memcpy(&req, req_bytes.data(), sizeof(req));
    if (req.magic != ANNOUNCE_MSG_ID) {
      return;
    }

    std::vector<uint8_t> reply_bytes;
    size_t num_scopes = 0;
    {
      std::lock_guard<std::mutex> lock(scopes_mutex);
      num_scopes = configured_scopes.size();
      size_t size =
          sizeof(announce_response_t) + num_scopes * sizeof(imscope_scope_config_t);
      reply_bytes.assign(size, 0);
      announce_response_t* msg =
          reinterpret_cast<announce_response_t*>(reply_bytes.data());
      msg->num_scopes = static_cast<int>(num_scopes);
      std::strncpy(msg->data_address, data_key_prefix.c_str(),
                   sizeof(msg->data_address) - 1);
      std::strncpy(msg->control_address, control_key.c_str(),
                   sizeof(msg->control_address) - 1);
      std::strncpy(msg->name, name.c_str(), sizeof(msg->name) - 1);
      for (size_t i = 0; i < num_scopes; i++) {
        std::strncpy(msg->scopes[i].name, configured_scopes[i].name.c_str(),
                     MAX_SCOPE_NAME_LEN - 1);
        std::strncpy(msg->scopes[i].group, configured_scopes[i].group.c_str(),
                     MAX_GROUP_NAME_LEN - 1);
        msg->scopes[i].type = configured_scopes[i].type;
        msg->scopes[i].domain = configured_scopes[i].domain;
      }
    }

    spdlog::debug(
        "ImscopeProducer: announced {} scopes to consumer. Data key: {} "
        "Control key: {}",
        num_scopes, data_key_prefix, control_key);

    try {
      query.reply(query.get_keyexpr(), zenoh::Bytes(std::move(reply_bytes)));
    } catch (const zenoh::ZException& e) {
      spdlog::error("ImscopeProducer: failed to reply to announce query: {}",
                    e.what());
    }
  }

  // Replies synchronously to a settings get-all/set request, mirroring the
  // NNG version's `SettingsCtx::settings_callback`. Settings ARE
  // implemented here (unlike the Rust producer crate's stub), reusing the
  // existing `SettingInfo`/`registered_settings` registry -- only the
  // transport call that reads the request and writes the reply changed.
  void handle_control_query(zenoh::Query& query) {
    auto payload = query.get_payload();
    if (!payload.has_value()) {
      return;
    }
    std::vector<uint8_t> req_bytes = payload->get().as_vector();
    if (req_bytes.size() < sizeof(setting_request_t)) {
      return;
    }
    setting_request_t req;
    std::memcpy(&req, req_bytes.data(), sizeof(req));

    std::vector<uint8_t> reply_bytes;
    if (req.magic == SETTING_REQ_GET_ALL) {
      std::lock_guard<std::mutex> lock(settings_mutex);
      size_t num_settings = registered_settings.size();
      size_t size = sizeof(setting_response_t);
      if (num_settings > 1) {
        size += (num_settings - 1) * sizeof(imscope_setting_t);
      } else if (num_settings == 0) {
        size -= sizeof(imscope_setting_t);
      }
      reply_bytes.assign(size, 0);
      setting_response_t* res =
          reinterpret_cast<setting_response_t*>(reply_bytes.data());
      res->magic = SETTING_REP_GET_ALL;
      res->status = 0;
      res->num_settings = static_cast<int32_t>(num_settings);
      for (size_t i = 0; i < num_settings; i++) {
        const auto& s = registered_settings[i];
        std::strncpy(res->settings[i].name, s.name.c_str(),
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
    } else if (req.magic == SETTING_REQ_SET) {
      std::lock_guard<std::mutex> lock(settings_mutex);
      bool found = false;
      int32_t status = -1;
      for (auto& s : registered_settings) {
        if (strncmp(s.name.c_str(), req.name, sizeof(req.name)) == 0) {
          found = true;
          if (s.type == req.type) {
            if (s.type == SETTING_TYPE_BOOL) {
              s.value.bval = req.value.bval != 0;
              if (s.bool_cb)
                s.bool_cb(s.value.bval);
            } else if (s.type == SETTING_TYPE_INT32) {
              s.value.ival = req.value.ival;
              if (s.int32_cb)
                s.int32_cb(s.value.ival);
            } else if (s.type == SETTING_TYPE_FLOAT) {
              s.value.fval = req.value.fval;
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
      reply_bytes.assign(sizeof(setting_response_t) - sizeof(imscope_setting_t),
                          0);
      setting_response_t* res =
          reinterpret_cast<setting_response_t*>(reply_bytes.data());
      res->magic = SETTING_REP_SET;
      res->status = status;
      res->num_settings = 0;
    } else {
      // Unrecognized request: drop it, the requester's `get()` eventually
      // times out.
      return;
    }

    try {
      query.reply(query.get_keyexpr(), zenoh::Bytes(std::move(reply_bytes)));
    } catch (const zenoh::ZException& e) {
      spdlog::error("ImscopeProducer: failed to reply to control query: {}",
                    e.what());
    }
  }

 public:
  // Sends the accumulated scalar buffer for `id` and clears it. Returns
  // IMSCOPE_SUCCESS even if no consumer is currently waiting on this scope
  // (the buffer stays queued for later).
  imscope_return_t send_accumulator_locked(
      int id, ScalarAccumulator& acc,
      std::chrono::steady_clock::time_point now) {
    std::optional<zenoh::Query> query;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return IMSCOPE_SUCCESS;
      }
      query.emplace(std::move(it->second));
      active_requests.erase(it);
    }

    auto start = std::chrono::high_resolution_clock::now();
    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * acc.buffer.size();
    std::vector<uint8_t> bytes(size, 0);
    scope_msg_t* msg = reinterpret_cast<scope_msg_t*>(bytes.data());
    msg->meta = {};
    msg->id = id;
    msg->data_size = acc.buffer.size() * sizeof(uint32_t);
    std::memcpy((void*)(msg + 1), acc.buffer.data(),
                sizeof(uint32_t) * acc.buffer.size());
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    imscope_return_t result = IMSCOPE_SUCCESS;
    try {
      query->reply(query->get_keyexpr(), zenoh::Bytes(std::move(bytes)));
    } catch (const zenoh::ZException& e) {
      spdlog::error(
          "ImscopeProducer: failed to reply scalar accumulator for scope "
          "{}: {}",
          id, e.what());
      result = IMSCOPE_ERROR_INTERNAL;
    }

    acc.buffer.clear();
    acc.last_send_time = now;

    return result;
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

  ImscopeProducer() {}

  ~ImscopeProducer() {
    stop_flush_thread();
    // Dropping the session undeclares all of its background queryables
    // (announce, control, and every scope's data queryable).
    session.reset();
  }

  void clear_scopes() {
    std::lock_guard<std::mutex> lock(scopes_mutex);
    configured_scopes.clear();
    {
      std::lock_guard<std::mutex> s_lock(scalar_mutex);
      scalar_accumulators.clear();
    }
  }

  // `data_address` is deprecated/unused: transport addressing is now the
  // Zenoh session plus the `imscope/<name>/...` key-expression prefix, not
  // a separate socket address. `announce_address` is reinterpreted as an
  // optional Zenoh locator to explicitly connect to (e.g.
  // "tcp/127.0.0.1:7447"); an empty string relies on Zenoh's default
  // scouting/discovery instead, mirroring the Rust producer's
  // `Option<&str>::None` convention.
  void connect(const char* data_address, const char* announce_address,
               const char* name) {
    stop_flush_thread();
    (void)data_address;

    this->name = name;

    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      active_requests.clear();
    }
    {
      std::lock_guard<std::mutex> lock(acquired_buffers_mutex);
      acquired_buffers.clear();
    }
    {
      std::lock_guard<std::mutex> lock(scalar_mutex);
      scalar_accumulators.clear();
    }

    // Drop the old session (if any) before opening a new one -- this
    // undeclares all previously-declared queryables.
    session.reset();

    try {
      session.emplace(open_zenoh_session(announce_address));
    } catch (const zenoh::ZException& e) {
      spdlog::error(
          "ImscopeProducer: failed to open Zenoh session (locator='{}'): {}",
          announce_address, e.what());
      return;
    }

    data_key_prefix = "imscope/" + this->name + "/data";
    control_key = "imscope/" + this->name + "/control";
    std::string announce_key = "imscope/" + this->name + "/announce";

    try {
      session->declare_background_queryable(
          announce_key,
          [this](zenoh::Query& query) { handle_announce_query(query); },
          []() {});
      session->declare_background_queryable(
          control_key,
          [this](zenoh::Query& query) { handle_control_query(query); },
          []() {});
    } catch (const zenoh::ZException& e) {
      spdlog::error(
          "ImscopeProducer: failed to declare announce/control queryables: "
          "{}",
          e.what());
      session.reset();
      return;
    }

    {
      std::lock_guard<std::mutex> lock(scopes_mutex);
      for (size_t i = 0; i < configured_scopes.size(); i++) {
        declare_data_queryable(static_cast<int>(i));
      }
    }

    spdlog::debug(
        "ImscopeProducer: producer '{}' listening on Zenoh (locator='{}')",
        this->name, announce_address);

    start_flush_thread();
  }

  imscope_return_t add_scope(const char* name, scope_type_t type,
                             const char* group = "",
                             scope_domain_t domain = SCOPE_DOMAIN_TIME) {
    int new_id = -1;
    {
      std::lock_guard<std::mutex> lock(scopes_mutex);
      for (size_t i = 0; i < configured_scopes.size(); i++) {
        if (configured_scopes[i].name == name) {
          return IMSCOPE_SUCCESS;
        }
      }
      scope_info_t scope_info = {name, group ? group : "", type, domain};
      configured_scopes.push_back(scope_info);
      new_id = static_cast<int>(configured_scopes.size() - 1);
    }
    declare_data_queryable(new_id);
    return IMSCOPE_SUCCESS;
  }

  int get_or_register_scope(const char* name, scope_type_t type,
                            const char* group = "",
                            scope_domain_t domain = SCOPE_DOMAIN_TIME) {
    int id = -1;
    bool is_new = false;
    {
      std::lock_guard<std::mutex> lock(scopes_mutex);
      for (size_t i = 0; i < configured_scopes.size(); i++) {
        if (configured_scopes[i].name == name) {
          return static_cast<int>(i);
        }
      }
      scope_info_t scope_info = {name, group ? group : "", type, domain};
      configured_scopes.push_back(scope_info);
      id = static_cast<int>(configured_scopes.size() - 1);
      is_new = true;
    }
    if (is_new) {
      declare_data_queryable(id);
    }
    return id;
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
    std::optional<zenoh::Query> query;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return IMSCOPE_ERROR_BUSY;
      }
      query.emplace(std::move(it->second));
      active_requests.erase(it);
    }

    auto start = std::chrono::high_resolution_clock::now();

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    std::vector<uint8_t> bytes(size, 0);
    scope_msg_t* msg = reinterpret_cast<scope_msg_t*>(bytes.data());
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    std::memcpy((void*)(msg + 1), data, sizeof(uint32_t) * num_samples);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    try {
      query->reply(query->get_keyexpr(), zenoh::Bytes(std::move(bytes)));
    } catch (const zenoh::ZException& e) {
      spdlog::error("ImscopeProducer: failed to reply to scope {} query: {}",
                    id, e.what());
      return IMSCOPE_ERROR_INTERNAL;
    }

    return IMSCOPE_SUCCESS;
  }

  void* acquire_buffer(int id, size_t num_samples) {
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      if (active_requests.find(id) == active_requests.end()) {
        return nullptr;
      }
    }

    size_t size = sizeof(scope_msg_t) + sizeof(uint32_t) * num_samples;
    std::lock_guard<std::mutex> lock(acquired_buffers_mutex);
    auto& buffer = acquired_buffers[id];
    buffer.assign(size, 0);
    scope_msg_t* msg = reinterpret_cast<scope_msg_t*>(buffer.data());
    msg->id = id;
    return (void*)(msg + 1);
  }

  imscope_return_t commit_buffer(int id, size_t num_samples, int frame,
                                 int slot, uint64_t timestamp) {
    std::optional<zenoh::Query> query;
    {
      std::lock_guard<std::mutex> lock(active_requests_mutex);
      auto it = active_requests.find(id);
      if (it == active_requests.end()) {
        return IMSCOPE_ERROR_BUSY;  // Should not happen if acquired
      }
      query.emplace(std::move(it->second));
      active_requests.erase(it);
    }

    std::vector<uint8_t> buffer;
    {
      std::lock_guard<std::mutex> lock(acquired_buffers_mutex);
      auto it = acquired_buffers.find(id);
      if (it == acquired_buffers.end()) {
        return IMSCOPE_ERROR_NOT_INITIALIZED;
      }
      buffer = std::move(it->second);
      acquired_buffers.erase(it);
    }

    auto start = std::chrono::high_resolution_clock::now();
    scope_msg_t* msg = reinterpret_cast<scope_msg_t*>(buffer.data());
    msg->id = id;
    msg->meta.frame = frame;
    msg->meta.slot = slot;
    msg->meta.timestamp = timestamp;
    msg->data_size = num_samples * sizeof(uint32_t);
    msg->time_taken_in_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::high_resolution_clock::now() - start)
            .count();

    try {
      query->reply(query->get_keyexpr(), zenoh::Bytes(std::move(buffer)));
    } catch (const zenoh::ZException& e) {
      spdlog::error(
          "ImscopeProducer: failed to reply to scope {} query (commit): {}",
          id, e.what());
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
