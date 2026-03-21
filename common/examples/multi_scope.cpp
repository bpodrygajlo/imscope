/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <spdlog/spdlog.h>
#include <unistd.h>
#include <complex>
#include <iostream>
#include <random>
#include <vector>
#include "imscope_producer.h"

int main() {
  spdlog::set_level(spdlog::level::debug);

  // Define two scopes: one for real data, one for IQ data
  imscope_scope_desc_t scopes[] = {
      {"RealNoise", scope_type_t::SCOPE_TYPE_REAL},
      {"IQSignal", scope_type_t::SCOPE_TYPE_IQ_DATA},
  };

  std::cout << "Initializing producer with 2 scopes..." << std::endl;
  imscope_init_producer("tcp://127.0.0.1:5556", "tcp://127.0.0.1:5557",
                        "multi_scope_example", scopes, 2);

  std::random_device rd;
  std::mt19937 gen(rd());
  std::uniform_int_distribution<int16_t> dis_real(0, 1000);
  std::uniform_int_distribution<int16_t> dis_iq(-500, 500);

  uint64_t timestamp = 0;
  int count = 0;

  while (count < 60) {
    // 1. Produce real data for scope 0
    std::vector<uint32_t> real_data(512);
    for (int i = 0; i < 512; i++) {
      // For SCOPE_TYPE_REAL, we send uint32_t. In many cases it might be
      // float cast to uint32_t or just integer values.
      real_data[i] = static_cast<uint32_t>(dis_real(gen));
    }
    imscope_try_send_data(real_data.data(), 0, 512, 0, 0, timestamp);

    // 2. Produce IQ data for scope 1
    // For SCOPE_TYPE_IQ_DATA, we pack I (16-bit) and Q (16-bit) into 32-bit uint32_t.
    std::vector<uint32_t> iq_data(512);
    for (int i = 0; i < 512; i++) {
      int16_t i_val = dis_iq(gen);
      int16_t q_val = dis_iq(gen);
      iq_data[i] = (static_cast<uint32_t>(static_cast<uint16_t>(i_val)) << 16) |
                   (static_cast<uint32_t>(static_cast<uint16_t>(q_val)));
    }
    imscope_try_send_data(iq_data.data(), 1, 512, 0, 0, timestamp);

    std::cout << "Sent step " << count++
              << " for both scopes (ts: " << timestamp << ")" << std::endl;

    timestamp += 512;
    sleep(1);
  }

  return 0;
}
