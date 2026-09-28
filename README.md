# imscope

`imscope` is a real-time signal and IQ data visualization tool designed for high-data-rate applications, such as 5G protocol stacks. It allows developers to visualize live signal streams concurrently using multiple interactive plotting methods.

The project features a **C++ core producer library**, a **native Rust producer crate**, and **three Rust-based clients** (a GPU-accelerated GUI, a full TUI, and a low-interactivity single-scope viewer).

---

## Architecture Overview

Producers and consumers all speak the same wire protocol over **[Zenoh](https://zenoh.io)** (Eclipse zenoh) — its reference implementation is Rust (used directly by `imscope-producer-rs` and the Rust clients), and the C++ side uses the official `zenoh-c`/`zenoh-cpp` bindings built from that same core, so both languages are first-class peers on the same network, discovering each other by producer name (no fixed ports to configure).

```
 [ C++ Data Producer ]              [ Native Rust Producer ]
 (e.g. 5G Stack / RF Simulator)     (imscope-producer-rs)
        │                                   │
        └───────────────┬───────────────────┘
                         ▼ (Zenoh: pub/sub-style query/reply)
 ┌──────────────────────────────────────┐
 │         Rust Clients                 │
 │  ┌────────────────────────────────┐  │
 │  │        imscope-gui             │  │
 │  │  (Dear ImGui + ImPlot + wgpu)  │  │
 │  └────────────────────────────────┘  │
 │  ┌────────────────────────────────┐  │
 │  │        imscope-tui             │  │
 │  │        (Ratatui)               │  │
 │  └────────────────────────────────┘  │
 │  ┌────────────────────────────────┐  │
 │  │        imscope-view            │  │
 │  │  (Ratatui, view-only, 1 scope) │  │
 │  └────────────────────────────────┘  │
 └──────────────────────────────────────┘
```

---

## Features

*   **Real-time Visualization:** Plot signal data with minimal latency as it is generated.
*   **Multiple Plot Modes:**
    *   **Scatter / Constellation (IQ):** Visualizes the complex symbol constellation.
    *   **RMS Power:** Plots root-mean-square power over samples.
    *   **Waveform:** Shows the real and imaginary components over time.
    *   **Histogram (1D):** Amplitude distribution of real/imaginary parts.
    *   **2D Density:** Bivariate heatmaps of IQ distribution.
*   **Remote Connection:** Asynchronous, non-blocking discovery and data transport using **Zenoh** — producers are found by name, with no fixed socket addresses to configure, over TCP or Zenoh's default local-network scouting.
*   **State Management:**
    *   **Signal Filters:** Live noise filtering based on magnitude cutoffs or sample percentages.
    *   **Data Stacking:** Custom sample stacking sizes for deeper visual analysis.
    *   **Group View:** Automatically merges and overlays plots of scopes belonging to the same group.

---

## Prerequisites

### 1. C++ Core Library (Producer)
To build the C++ producer library, examples, and tests:
*   **CMake** (>= 3.10)
*   **C++17 Compiler** (GCC, Clang, or MSVC)
*   **Rust Toolchain** (Cargo) — `zenoh-c` is built from the Rust `zenoh` crate via CPM/Cargo at configure time; there's no separate system package.
*   **Zenoh** (`zenoh-c` / `zenoh-cpp`, automatically fetched via CPM)

### 2. Rust Clients & Producer (GUI, TUI, view, and `imscope-producer-rs`)
To build the graphical/terminal clients or the native Rust producer crate:
*   **Rust Toolchain** (Cargo, standard edition)
*   **Graphics Backend** (Vulkan, Metal, or OpenGL drivers for GPU rendering in the GUI)

---

## Build Instructions

### Building the C++ Core & Examples
```bash
mkdir build
cd build
cmake ..
make -j
```
This compiles the core producer library and testing binaries (e.g., `settings_example`).

### Building the Rust Clients
From the project root directory, compile the TUI and GUI clients using Cargo:
```bash
cargo build --release
```
The compiled binaries will be located in `target/release/imscope-gui`, `target/release/imscope-tui`, and `target/release/imscope-view`.

---

## Usage

### 1. Starting the Data Producer
Run the settings/data example to start generating dummy signals:
```bash
./build/common/examples/settings_example
```

### 2. Launching the GUI Client (GPU-accelerated)
Run the Rust GUI client:
```bash
cargo run --release --bin imscope-gui
# Or run the compiled binary:
./target/release/imscope-gui
```
*   **Control Center:** Enter the Producer name (default `imscope`) — and, optionally, an explicit Zenoh locator if you're not relying on local-network scouting — then click **Connect**.
*   **Layout Setup:** Switch between **1 Pane** or **2 Panes** layout to monitor multiple scopes side-by-side.
*   **Live Parameter Controls:** Toggle Auto-Collect, modify sample filters, adjust Stacking Size, and dynamically update Producer parameters under the **Producer Dynamic Settings** collapsible tree.
*   **Visual Tabs:** Switch between visual tabs (**Scatter**, **RMS Power**, **Waveform**, **Histogram**, and **2D Density**) to view the data.

### 3. Launching the TUI Client (Terminal-based)
Run the Rust terminal client:
```bash
cargo run --release --bin imscope-tui
# Or run the compiled binary:
./target/release/imscope-tui
```
*   Use standard terminal mouse interaction or keyboard shortcuts to toggle views.
*   Configure signal filters and stacking parameters directly from the sidebar.

### 4. Launching the low-interactivity single-scope viewer
`imscope-view` is a minimal, view-only TUI: it connects, shows one scope, and has no editable
settings/filters/panes. It's meant to be left running in a terminal, or started several times
at once (one per terminal pane/window) to watch multiple channels from the same producer
side by side:
```bash
# Discover scope names first:
imscope-view --producer "My Producer" --list

# Then, e.g. in three separate terminal panes (tmux, terminal tabs, etc.):
imscope-view --producer "My Producer" --scope "Channel A"
imscope-view --producer "My Producer" --scope "Channel B" --plot spectrum
imscope-view --producer "My Producer" --scope "Channel C" --plot histogram
```
Each instance is an independent process that connects to the producer on its own, so running
several concurrently needs no special setup — just point them at the same `--producer` name
(add `--zenoh-connect <locator>` if you're not relying on local-network scouting) with
different `--scope` values. Keys: `1`-`5` switch the displayed plot (Scatter/RMS/
Waveform/Histogram/Spectrum), `q`/`Esc`/`Ctrl-C` quits.

---

## Integrating the Producer API
To send signal data from your own application, include the producer header:
```c
#include "imscope_producer.h"
```

1.  **Initialize the Producer:**
    ```c
    imscope_scope_desc_t scopes[] = {
        {"Channel A", SCOPE_TYPE_IQ_DATA, "Group 1"},
        {"Channel B", SCOPE_TYPE_IQ_DATA, "Group 1"}
    };

    imscope_init_producer(
        "",             // Unused/deprecated (kept for source compatibility)
        "",             // Optional Zenoh locator to connect to, e.g. "tcp/127.0.0.1:7447";
                        // "" relies on Zenoh's default local-network scouting
        "My Producer",  // Producer name -- how clients find you (imscope/<name>/...)
        scopes,
        2
    );
    ```

2.  **Send Data:**
    ```c
    std::vector<uint32_t> iq_samples = ...; // Interleaved 16-bit complex IQ
    imscope_try_send_data(
        iq_samples.data(),
        0,                 // Scope ID
        iq_samples.size(), // Sample count
        frame,
        slot,
        timestamp
    );
    ```

---

## Native Rust Producer (`imscope-producer-rs`)
For pure-Rust applications, `producer/` provides a native Rust producer that speaks the exact
same Zenoh wire protocol — no FFI into the C++ library required:
```rust
use imscope_producer_rs::{NRmetadata, Producer, ScopeDesc, ScopeType};

let scopes = [ScopeDesc::new("Channel A", ScopeType::IqData)];
let producer = Producer::new(None, "My Producer", &scopes)?; // None = default scouting

// In your data loop:
producer.try_send_iq(0, &real_samples, &imag_samples, NRmetadata::default())?;
```
See `producer/examples/dual_scope.rs` for a complete runnable example. Note: this crate's
dynamic settings protocol (the equivalent of `imscope_register_setting_*`) isn't implemented
yet — see the doc comment on `spawn_control_queryable` in `producer/src/producer.rs`.

---

## License

This project is licensed under the **MIT License**. See the [LICENSE](LICENSE) file for the full text.
