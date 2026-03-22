/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <spdlog/spdlog.h>
#include <unistd.h>
#include <iostream>
#include <random>
#include "imscope_producer.h"

int main() {
  spdlog::set_level(spdlog::level::debug);
  imscope_scope_desc_t scopes[] = {
      {"test", scope_type_t::SCOPE_TYPE_REAL},
  };
  imscope_init_producer("tcp://127.0.0.1:5556", "tcp://127.0.0.1:5557", "test",
                        scopes, 1);
  int data_produced = 0;
  std::random_device rd;
  std::mt19937 gen(rd());
  std::uniform_int_distribution<uint16_t> dis(0, 65535);
  uint64_t timestamp = 0;
  ssize_t nsec = 30;
  while (nsec-- > 0) {
    uint16_t data[1024];
    for (int i = 0; i < 1024; i++) {
      data[i] = dis(gen) / 10;
    }
    std::cout << "Producing data " << data_produced++ << std::endl;
    imscope_try_send_data((uint32_t*)data, 0, 512, 0, 0, timestamp);
    timestamp += 512;
    sleep(1);
  }
}
