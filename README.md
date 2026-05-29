# imscope

`imscope` is a real-time signal and IQ data visualization tool designed for high-data-rate applications, such as 5G protocol stacks. It allows developers to visualize live signal streams concurrently using multiple interactive plotting methods.

The project features a **C++ core producer library** and **two Rust-based clients** (a TUI and a GPU-accelerated GUI).

---

## Architecture Overview

```
 [ C++ Data Producer ] (e.g. 5G Stack / RF Simulator)
        │
        ▼ (NNG Protocol)
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
*   **Remote Connection:** Asynchronous, non-blocking discovery and data transport using the **NNG** library.
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
*   **NNG** (automatically fetched or system-installed)

### 2. Rust Clients (GUI & TUI)
To build the graphical or terminal clients:
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
The compiled binaries will be located in `target/release/imscope-gui` and `target/release/imscope-tui`.

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
*   **Control Center:** Configure the Announcer URL (default `tcp://127.0.0.1:5557`) and click **Connect**.
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
        "tcp://0.0.0.0:5558", // Data socket bind address
        "tcp://0.0.0.0:5559", // Announce socket bind address
        "My Producer",
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

## License

This project is licensed under the **MIT License**. See the [LICENSE](LICENSE) file for the full text.
