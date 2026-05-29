# imscope - AI Agent Context

`imscope` is a real-time signal visualization tool designed for high-throughput IQ data (e.g., 5G stacks). It utilizes **NNG** for communication and **Dear ImGui** / **ImPlot** / **Ratatui** for the graphical and terminal interfaces.

## Tech Stack
- **Languages:** C++17 (producer library), Rust (TUI & GUI clients)
- **Communication:** NNG
- **GUI client:** Rust, `dear-app`, `dear-imgui-rs`, `dear-implot`, OpenGL/Vulkan/Metal graphics backends
- **TUI client:** Rust, `ratatui`, `crossterm`
- **Logging:** `spdlog` (C++ producer), standard Rust logging macros

## Project Structure & File Descriptions

### Core C++ Library (`common/`)
- [common/CMakeLists.txt](common/CMakeLists.txt): Build system for the core components.
- [common/include/imscope_common.h](common/include/imscope_common.h): Defines common data structures used across the producer and consumer, such as `NRmetadata` (frame/slot/timestamp), `scope_msg_t` types, and shared constants/enums.
- [common/include/imscope_producer.h](common/include/imscope_producer.h): Public C-style API for data producers. Provides functions to initialize a producer (`imscope_init_producer`) and send IQ data (`imscope_try_send_data`).
- [common/include/imscope_consumer.h](common/include/imscope_consumer.h): C++ interface for data consumers. Defines the `ImscopeConsumer` class for connecting to producers and requesting data snapshots.
- [common/include/imscope_tools.h](common/include/imscope_tools.h): Utility classes for processing and storing snapshots (MovingAverageTimer, IQSnapshot, VectorSnapshot).
- [common/src/imscope_producer.cpp](common/src/imscope_producer.cpp): Implements the producer logic (Control, Data, and Announce sockets).
- [common/src/imscope_consumer.cpp](common/src/imscope_consumer.cpp): Implements the `ImscopeConsumer` class.
- [common/src/imscope_internal.h](common/src/imscope_internal.h) / [common/src/imscope_internal.cpp](common/src/imscope_internal.cpp): Internal utilities for NNG socket creation and error handling.
- [common/src/imscope_tools.cpp](common/src/imscope_tools.cpp): Implementation of data processing tools.
- [common/test/](common/test/): Unit and integration tests for core logic.

### Rust Clients (`src/`)
- [src/bin/gui.rs](src/bin/gui.rs): Entry point for the immediate-mode GUI client. Coordinates layout windows (Control Center + independent Scope Panes), producer setting edits, signal filters, and plots using `dear-app` / `dear-imgui-rs` / `dear-implot`.
- [src/bin/tui.rs](src/bin/tui.rs): Entry point for the terminal user interface client built with `ratatui`.
- [src/consumer.rs](src/consumer.rs): Rust wrapper for NNG sockets, worker thread loop logic, and snapshot/filter/stacking data structures.
- [Cargo.toml](Cargo.toml): Declares Rust dependencies (`dear-app`, `dear-imgui-rs`, `dear-implot`, `ratatui`, `nng`).

### Root Files
- [CMakeLists.txt](CMakeLists.txt): Root build configuration for C++ modules.
- [README.md](README.md): General documentation, installation, and usage guide.
- [.pre-commit-config.yaml](.pre-commit-config.yaml): Code quality checks.
