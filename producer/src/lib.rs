/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! A native-Rust producer for the imscope Zenoh wire protocol.
//!
//! This crate is the Rust counterpart to the C++ producer library in
//! `common/` (`imscope_init_producer` / `imscope_try_send_data` and
//! friends): it lets a pure-Rust application publish scope data that shows
//! up unmodified in `imscope-gui`, `imscope-tui`, `imscope-view`, and the
//! C++ consumer, with zero protocol changes.
//!
//! Transport note: as of this crate's Zenoh migration, the C++ producer and
//! Rust consumer (`src/consumer.rs`) still speak the older NNG transport --
//! they have not been ported yet (see the multi-stage migration plan this
//! crate is Stage A of). This crate's [`Producer`] only interoperates with
//! peers that also speak Zenoh using the same `imscope/<name>/...`
//! key-expression scheme.
//!
//! See [`Producer`] for the main entry point, and the `dual_scope` example
//! for a minimal end-to-end publisher.
//!
//! Control/settings note: the dynamic settings protocol
//! (`imscope_register_setting_bool/int32/float` on the C++ side) is stubbed
//! here -- see the doc comment on `spawn_control_worker` in `producer.rs`.

mod producer;
pub mod wire;

pub use producer::{Producer, ProducerError, ScopeDesc, TrySendError};
pub use wire::{NRmetadata, ScopeDomain, ScopeType};
