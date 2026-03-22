/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <chrono>
#include <cmath>
#include <iostream>
#include <thread>
#include <vector>
#include "imscope_producer.h"

int main() {
  imscope_scope_desc_t scopes[] = {{"zero_copy_sine", SCOPE_TYPE_REAL}};

  imscope_return_t rv =
      imscope_init_producer("tcp://127.0.0.1:5555", "tcp://127.0.0.1:5556",
                            "ZeroCopyProducer", scopes, 1);
  if (rv != IMSCOPE_SUCCESS) {
    std::cerr << "Failed to initialize producer" << std::endl;
    return 1;
  }

  std::cout << "Zero-copy producer started. Sending sine wave..." << std::endl;

  const size_t num_samples = 1024;
  int frame = 0;
  int slot = 0;

  while (true) {
    // 1. Acquire buffer
    void* buffer = imscope_acquire_send_buffer(0, num_samples);
    if (buffer) {
      uint32_t* data = static_cast<uint32_t*>(buffer);

      // 2. Fill buffer directly
      for (size_t i = 0; i < num_samples; ++i) {
        float val =
            std::sin(2.0f * M_PI * (i + frame * num_samples) / 10000.0f);
        // Convert float to uint32 bit representation or actual scaled value
        data[i] = *reinterpret_cast<uint32_t*>(&val);
      }

      // 3. Commit buffer
      imscope_commit_send_buffer(0, num_samples, frame, slot, 0);

      slot = (slot + 1) % 20;
      if (slot == 0)
        frame++;
    } else {
      // Buffer busy, wait a bit
      std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }

    std::this_thread::sleep_for(std::chrono::milliseconds(10));
  }

  return 0;
}
