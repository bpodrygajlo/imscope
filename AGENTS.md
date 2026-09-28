# imscope - AI Agent Context

`imscope` is a real-time signal visualization tool designed for high-throughput IQ data (e.g., 5G stacks). It utilizes **Zenoh** (Eclipse zenoh) for communication and **Dear ImGui** / **ImPlot** / **Ratatui** for the graphical and terminal interfaces.

## Tech Stack
- **Languages:** C++17 (producer library), Rust (native producer crate, TUI/GUI/view clients)
- **Communication:** Zenoh -- Rust-origin reference implementation (`zenoh` crate) used by the Rust side; C++ side uses the official `zenoh-c`/`zenoh-cpp` bindings built from that same core. Producers and consumers discover each other by producer name via Zenoh key expressions (`imscope/<name>/announce|control|data/<scope_id>`), not fixed socket addresses.
- **GUI client:** Rust, `dear-app`, `dear-imgui-rs`, `dear-implot`, OpenGL/Vulkan/Metal graphics backends
- **TUI client:** Rust, `ratatui`, `crossterm`
- **Logging:** `spdlog` (C++ producer), standard Rust logging macros

## Project Structure & File Descriptions

### Core C++ Library (`common/`)
- [common/CMakeLists.txt](common/CMakeLists.txt): Build system for the core components.
- [common/include/imscope_common.h](common/include/imscope_common.h): Defines common data structures used across the producer and consumer, such as `NRmetadata` (frame/slot/timestamp), `scope_msg_t` types, and shared constants/enums.
- [common/include/imscope_producer.h](common/include/imscope_producer.h): Public C-style API for data producers. Provides functions to initialize a producer (`imscope_init_producer`) and send IQ data (`imscope_try_send_data`). Signature kept unchanged across the NNG->Zenoh migration for source compatibility: `announce_address` is now an optional Zenoh locator (empty = default scouting), `data_address` is unused/deprecated.
- [common/include/imscope_consumer.h](common/include/imscope_consumer.h): C++ interface for data consumers. Defines the `ImscopeConsumer` class for connecting to producers and requesting data snapshots. `connect()`'s first parameter is now the producer's name (the Zenoh key-expression prefix), with an optional second parameter for an explicit Zenoh locator.
- [common/src/imscope_producer.cpp](common/src/imscope_producer.cpp): Implements the producer logic over Zenoh -- one `Session` with a `Queryable` each for announce/control, plus one per scope for data, using a pending-`Query` map (clone-and-store on request, reply-and-erase on `imscope_try_send_data`) for non-blocking drop-if-nobody's-waiting semantics.
- [common/src/imscope_consumer.cpp](common/src/imscope_consumer.cpp): Implements the `ImscopeConsumer` class over Zenoh (`session.get()` against the announce/control/data key expressions).
- [common/src/imscope_internal.h](common/src/imscope_internal.h) / [common/src/imscope_internal.cpp](common/src/imscope_internal.cpp): Internal Zenoh session-helper utilities and error handling.
- [common/test/](common/test/): Unit and integration tests for core logic.

### Rust Clients (`src/`)
- [src/bin/gui.rs](src/bin/gui.rs): Entry point for the immediate-mode GUI client. Coordinates layout windows (Control Center + independent Scope Panes), producer setting edits, signal filters, and plots using `dear-app` / `dear-imgui-rs` / `dear-implot`.
- [src/bin/tui.rs](src/bin/tui.rs): Entry point for the full interactive terminal client built with `ratatui` (panes, settings, filters, stacking, groups).
- [src/bin/view.rs](src/bin/view.rs): Entry point for `imscope-view`, a low-interactivity, view-only single-scope TUI. Meant to be run standalone or several at once (one per terminal, each with a different `--scope`) to watch multiple channels from one producer side by side.
- [src/tui_render.rs](src/tui_render.rs): Shared Ratatui rendering for a single scope's plot (Scatter/RMS/Waveform/Histogram/Spectrum + metadata footer), used by both `imscope-tui` and `imscope-view`.
- [src/consumer.rs](src/consumer.rs): Rust wrapper around a Zenoh `Session` (queries against the announce/control/data key expressions), worker thread loop logic, and snapshot/filter/stacking data structures.
- [Cargo.toml](Cargo.toml): Workspace manifest (`[workspace] members = ["producer"]`) that also declares the `imscope` package itself (root package as a workspace member): its Rust dependencies (`dear-app`, `dear-imgui-rs`, `dear-implot`, `ratatui`, `zenoh`) and the `imscope-gui`/`imscope-tui`/`imscope-view` binary targets.

### Native Rust Producer (`producer/`)
- [producer/Cargo.toml](producer/Cargo.toml): Manifest for the `imscope-producer-rs` workspace member crate (lib name `imscope_producer_rs`); depends on `zenoh`.
- [producer/src/lib.rs](producer/src/lib.rs): Crate root; re-exports the public API (`Producer`, `ScopeDesc`, `ScopeType`, `ScopeDomain`, `NRmetadata`, `TrySendError`, `ProducerError`).
- [producer/src/wire.rs](producer/src/wire.rs): Byte-level encode/decode for the imscope wire protocol (announce response, scope request/reply, setting request/reply), pinned byte-for-byte to `common/include/imscope_common.h` and `src/consumer.rs`'s parsers -- unchanged by the NNG->Zenoh migration, since only the transport carrying these bytes changed.
- [producer/src/producer.rs](producer/src/producer.rs): The `Producer` type: opens one Zenoh `Session` and declares a `Queryable` each for the announce/control key expressions, plus one per scope for data (`imscope/<name>/announce|control|data/<scope_id>`), using a pending-`Query` map (clone-and-store on request, reply-and-erase on `try_send_*`) for the same non-blocking drop-if-nobody's-waiting semantics the old NNG version had. The control/settings protocol is currently stubbed (always reports zero settings) -- see the doc comment on `spawn_control_queryable`.
- [producer/examples/dual_scope.rs](producer/examples/dual_scope.rs): Publishes a `SCOPE_TYPE_REAL` sine wave and a `SCOPE_TYPE_IQ_DATA` noisy phasor for a quick smoke test against `imscope-tui`/`imscope-gui`/`imscope-view`, analogous to `common/examples/sine_wave.cpp` / `multi_scope.cpp`.
- [producer/tests/integration.rs](producer/tests/integration.rs): Live round-trip test: starts a real `Producer` and drives it with a hand-rolled Zenoh client that queries it exactly like `src/consumer.rs` does, independent of the `imscope` crate.

### Root Files
- [CMakeLists.txt](CMakeLists.txt): Root build configuration for C++ modules.
- [README.md](README.md): General documentation, installation, and usage guide.
- [.pre-commit-config.yaml](.pre-commit-config.yaml): Code quality checks.
- [scripts/manual-test-tmux.sh](scripts/manual-test-tmux.sh): Manual smoke test -- builds the project and opens a tmux session with one producer (`imscope-producer-rs`'s `dual_scope` example) and two `imscope-view` consumers on different scopes, side by side.

## Commit Policy
- Do not add AI co-authorship trailers (e.g. `Co-Authored-By: Claude ...`) to commit messages or PR descriptions in this repo.
