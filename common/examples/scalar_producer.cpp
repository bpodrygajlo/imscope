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
#include "imscope_producer.h"

// Simulates a few scalar metrics a radio stack might expose:
//   - bler       (float 0-100%): block error rate, slow triangle wave
//   - snr_db     (float):        SNR estimate, slow sine
//   - harq_retx  (int32):        HARQ retransmission counter, increments on
//                                simulated error events
//   - mcs_index  (int32):        modulation-coding scheme index (0-28), tracks
//                                a step function that adapts to simulated SNR

int main() {
  spdlog::set_level(spdlog::level::info);

  std::cout << "Starting scalar producer example..." << std::endl;
  std::cout << "Connect the TUI to tcp://127.0.0.1:5557" << std::endl;

  imscope_init_producer("tcp://127.0.0.1:5556", "tcp://127.0.0.1:5557",
                        "scalar_example", nullptr, 0);

  double phase = 0.0;
  int32_t harq_retx = 0;
  int tick = 0;

  while (true) {
    // BLER: triangle wave 0..100
    float bler = 50.0f + 50.0f * static_cast<float>(std::sin(phase * 0.3));

    // SNR: sine wave 5..30 dB
    float snr_db = 17.5f + 12.5f * static_cast<float>(std::sin(phase));

    // MCS: step function derived from SNR (higher SNR → higher MCS)
    int32_t mcs_index = static_cast<int32_t>((snr_db - 5.0f) / 25.0f * 28.0f);
    if (mcs_index < 0)
      mcs_index = 0;
    if (mcs_index > 28)
      mcs_index = 28;

    // HARQ retransmissions: increment when BLER is high
    if (bler > 60.0f && (tick % 7 == 0)) {
      harq_retx++;
    }

    imscope_try_send_float_by_group(bler, "bler", "radio_metrics");
    imscope_try_send_float_by_group(snr_db, "snr_db", "radio_metrics");
    imscope_try_send_int32_by_group(harq_retx, "harq_retx", "harq");
    imscope_try_send_int32_by_group(mcs_index, "mcs_index", "harq");

    phase += 0.05;
    if (phase > 2.0 * M_PI)
      phase -= 2.0 * M_PI;

    tick++;
    if (tick % 100 == 0) {
      std::cout << "\rbler=" << bler << "% snr=" << snr_db
                << "dB mcs=" << mcs_index << " harq_retx=" << harq_retx << "   "
                << std::flush;
    }

    usleep(10000);  // 10ms → ~100 values/s per scope
  }

  return 0;
}
