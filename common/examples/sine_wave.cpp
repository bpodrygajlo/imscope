/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <spdlog/spdlog.h>
#include <unistd.h>
#include <cmath>
#include <iostream>
#include <vector>
#include "imscope_producer.h"

int main() {
  spdlog::set_level(spdlog::level::info);

  imscope_scope_desc_t scopes[] = {
      {"SineWave", scope_type_t::SCOPE_TYPE_REAL},
  };

  std::cout << "Starting SineWave example..." << std::endl;
  imscope_init_producer("tcp://127.0.0.1:5556", "tcp://127.0.0.1:5557",
                        "sine_wave_example", scopes, 1);

  uint64_t timestamp = 0;
  double phase = 0.0;
  const double freq = 0.01;  // frequency of the sine wave
  const int samples_per_batch = 1024;

  while (true) {
    std::vector<uint32_t> data(samples_per_batch);
    for (int i = 0; i < samples_per_batch; i++) {
      // Generate a sine wave scaled to 1000 range
      double val = 500.0 + 400.0 * std::sin(phase);
      data[i] = static_cast<uint32_t>(val);
      phase += 2.0 * M_PI * freq;
      if (phase > 2.0 * M_PI)
        phase -= 2.0 * M_PI;
    }

    // Send data to scope 0
    imscope_return_t ret = imscope_try_send_data(
        data.data(), 0, samples_per_batch, 0, 0, timestamp);
    if (ret == IMSCOPE_ERROR_BUSY) {
      // Scope busy, just wait a bit and retry
      usleep(10000);
      continue;
    }

    std::cout << "\rProduced batch at timestamp: " << timestamp << std::flush;
    timestamp += samples_per_batch;

    // Control the flow a bit
    usleep(50000);  // 50ms sleep
  }

  return 0;
}
