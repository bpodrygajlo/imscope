/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include "imscope_internal.h"

#include <iostream>

std::string g_test_extra_listen_locator;
bool g_test_disable_multicast_scouting = false;

zenoh::Session open_zenoh_session(const char* locator) {
  auto config = zenoh::Config::create_default();
  if (locator != nullptr && locator[0] != '\0') {
    // Mirrors the Rust side's `config.insert_json5("connect/endpoints",
    // &format!("[{loc:?}]"))` (see `producer/src/producer.rs::new` and
    // `src/consumer.rs::open_session`): a JSON5 array containing the single
    // explicit endpoint to dial, instead of relying on default scouting.
    std::string locator_json5 = std::string("[\"") + locator + "\"]";
    config.insert_json5("connect/endpoints", locator_json5);
  }
  if (!g_test_extra_listen_locator.empty()) {
    std::string listen_json5 = std::string("[\"") + g_test_extra_listen_locator + "\"]";
    config.insert_json5("listen/endpoints", listen_json5);
  }
  if (g_test_disable_multicast_scouting) {
    config.insert_json5("scouting/multicast/enabled", "false");
  }
  return zenoh::Session::open(std::move(config));
}

void print_announce_response(announce_response_t* msg) {
  std::cout << "" << msg->name << std::endl;
  std::cout << "Data address: " << msg->data_address << std::endl;
  std::cout << "---------------------------------" << std::endl;
  std::cout << "Number of scopes: " << msg->num_scopes << std::endl;
  for (int i = 0; i < msg->num_scopes; i++) {
    const char* type_str = "UNKNOWN";
    switch (msg->scopes[i].type) {
      case SCOPE_TYPE_REAL:
        type_str = "REAL";
        break;
      case SCOPE_TYPE_IQ_DATA:
        type_str = "IQ_DATA";
        break;
      case SCOPE_TYPE_INT32:
        type_str = "INT32";
        break;
      case SCOPE_TYPE_FLOAT:
        type_str = "FLOAT";
        break;
    }
    std::cout << "  Scope " << i << ": " << msg->scopes[i].name
              << " Type: " << type_str << std::endl;
  }
}
