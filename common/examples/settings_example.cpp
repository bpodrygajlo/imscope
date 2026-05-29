/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <spdlog/spdlog.h>
#include <unistd.h>
#include <algorithm>
#include <atomic>
#include <cmath>
#include <iostream>
#include <vector>
#include "imscope_producer.h"

// Thread-safe settings state using std::atomic
static std::atomic<bool> g_enable_generation{true};
static std::atomic<int32_t> g_amplitude{400};
static std::atomic<float> g_frequency{0.01f};

void on_enable_changed(bool val) {
  g_enable_generation.store(val);
  spdlog::info("Setting 'enable_generation' changed to {}", val);
}

void on_amplitude_changed(int32_t val) {
  g_amplitude.store(val);
  spdlog::info("Setting 'amplitude' changed to {}", val);
}

void on_frequency_changed(float val) {
  g_frequency.store(val);
  spdlog::info("Setting 'frequency' changed to {}", val);
}

int main() {
  spdlog::set_level(spdlog::level::info);

  imscope_scope_desc_t scopes[] = {
      {"ControlledWave", scope_type_t::SCOPE_TYPE_REAL},
  };

  std::cout << "Starting ControlledWave (Settings Example)..." << std::endl;

  // 1. Initialize the producer
  imscope_return_t ret =
      imscope_init_producer("tcp://127.0.0.1:5556", "tcp://127.0.0.1:5557",
                            "settings_example", scopes, 1);
  if (ret != IMSCOPE_SUCCESS) {
    spdlog::error("Failed to initialize producer");
    return 1;
  }

  // 2. Register dynamic settings with callbacks
  imscope_register_setting_bool("enable_generation", g_enable_generation.load(),
                                on_enable_changed);
  imscope_register_setting_int32("amplitude", g_amplitude.load(),
                                 on_amplitude_changed);
  imscope_register_setting_float("frequency", g_frequency.load(),
                                 on_frequency_changed);

  uint64_t timestamp = 0;
  double phase = 0.0;
  const int samples_per_batch = 1024;

  std::cout
      << "Producer initialized. Registering settings:\n"
      << "  - 'enable_generation' (Bool): Default = true\n"
      << "  - 'amplitude' (Int32): Default = 400\n"
      << "  - 'frequency' (Float): Default = 0.01\n"
      << "Connect the imscope TUI to interact with these settings dynamically!"
      << std::endl;

  while (true) {
    std::vector<uint32_t> data(samples_per_batch);

    // Read atomic values
    bool enabled = g_enable_generation.load();
    int32_t amp = g_amplitude.load();
    float freq = g_frequency.load();

    for (int i = 0; i < samples_per_batch; i++) {
      if (enabled) {
        // Generate two int16_t samples for each uint32_t element
        double val1 = static_cast<double>(amp) * std::sin(phase);
        phase += 2.0 * M_PI * static_cast<double>(freq);
        if (phase > 2.0 * M_PI)
          phase -= 2.0 * M_PI;

        double val2 = static_cast<double>(amp) * std::sin(phase);
        phase += 2.0 * M_PI * static_cast<double>(freq);
        if (phase > 2.0 * M_PI)
          phase -= 2.0 * M_PI;

        int16_t s1 = static_cast<int16_t>(std::clamp(val1, -32768.0, 32767.0));
        int16_t s2 = static_cast<int16_t>(std::clamp(val2, -32768.0, 32767.0));

        data[i] = (static_cast<uint32_t>(static_cast<uint16_t>(s1))) |
                  (static_cast<uint32_t>(static_cast<uint16_t>(s2)) << 16);
      } else {
        data[i] = 0;
      }
    }

    // Send data to scope 0
    ret = imscope_try_send_data(data.data(), 0, samples_per_batch, 0, 0,
                                timestamp);
    if (ret == IMSCOPE_ERROR_BUSY) {
      // Scope busy, just wait a bit and retry
      usleep(10000);
      continue;
    }

    timestamp += 2 * samples_per_batch;
    usleep(50000);  // 50ms sleep
  }

  imscope_cleanup_producer();
  return 0;
}
