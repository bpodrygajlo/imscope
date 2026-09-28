/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! Publishes two synthetic scopes for a quick side-by-side smoke test
//! against `imscope-tui` / `imscope-view` / the C++ consumer, analogous to
//! `common/examples/multi_scope.cpp` and `common/examples/sine_wave.cpp`:
//!
//! - `SineWave` (`SCOPE_TYPE_REAL`): a clean sine wave.
//! - `NoisyIQ` (`SCOPE_TYPE_IQ_DATA`): a rotating IQ phasor plus uniform noise.
//!
//! Run with:
//! ```text
//! cargo run -p imscope-producer-rs --example dual_scope
//! ```
//! then point a Zenoh-speaking consumer at producer name
//! `rust_dual_scope_example` (i.e. the `imscope/rust_dual_scope_example/...`
//! key-expression prefix), relying on Zenoh's default scouting to find this
//! process on the local network.

use std::f64::consts::PI;
use std::thread;
use std::time::Duration;

use imscope_producer_rs::{NRmetadata, Producer, ScopeDesc, ScopeType, TrySendError};

const SAMPLES_PER_BATCH: usize = 1024;

fn main() {
    let scopes = [
        ScopeDesc::new("SineWave", ScopeType::Real),
        ScopeDesc::new("NoisyIQ", ScopeType::IqData),
    ];

    println!("Starting dual_scope producer example...");
    let producer =
        Producer::new(None, "rust_dual_scope_example", &scopes).expect("failed to start producer");

    let sine_id = producer.scope_id_by_name("SineWave").unwrap();
    let iq_id = producer.scope_id_by_name("NoisyIQ").unwrap();

    println!(
        "Producer running as \"rust_dual_scope_example\" (relying on Zenoh default scouting). Key expressions: imscope/rust_dual_scope_example/{{announce,control,data/<scope_id>}}"
    );

    let mut phase = 0.0f64;
    let freq = 0.01f64;
    let mut timestamp: u64 = 0;
    // Simple xorshift PRNG so the example has no extra dependencies.
    let mut rng_state: u64 = 0x2545F4914F6CDD1D;
    let mut next_rand = move || {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };

    loop {
        let mut sine_samples = vec![0i16; SAMPLES_PER_BATCH];
        for s in sine_samples.iter_mut() {
            let val = 400.0 * (phase).sin();
            phase += 2.0 * PI * freq;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
            *s = val.clamp(-32768.0, 32767.0) as i16;
        }

        let mut iq_real = vec![0i16; SAMPLES_PER_BATCH];
        let mut iq_imag = vec![0i16; SAMPLES_PER_BATCH];
        for i in 0..SAMPLES_PER_BATCH {
            let angle = 2.0 * PI * (i as f64) / (SAMPLES_PER_BATCH as f64);
            let noise_i = (next_rand() % 100) as i16 - 50;
            let noise_q = (next_rand() % 100) as i16 - 50;
            iq_real[i] = (500.0 * angle.cos()) as i16 + noise_i;
            iq_imag[i] = (500.0 * angle.sin()) as i16 + noise_q;
        }

        let meta = NRmetadata {
            frame: 0,
            slot: 0,
            timestamp,
        };

        match producer.try_send_real(sine_id, &sine_samples, meta) {
            Ok(()) | Err(TrySendError::NoPendingRequest) => {}
            Err(e) => eprintln!("SineWave send error: {e}"),
        }
        match producer.try_send_iq(iq_id, &iq_real, &iq_imag, meta) {
            Ok(()) | Err(TrySendError::NoPendingRequest) => {}
            Err(e) => eprintln!("NoisyIQ send error: {e}"),
        }

        timestamp += SAMPLES_PER_BATCH as u64;
        thread::sleep(Duration::from_millis(50));
    }
}
