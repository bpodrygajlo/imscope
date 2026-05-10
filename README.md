# imscope

imscope is a real-time IQ data visualization tool designed for high-data-rate applications, such as 5G stacks. It allows developers to visualize signal data in real-time using various plot types.

## Features

*   **Real-time Visualization:** Plot IQ data as it is produced.
*   **Multiple Plot Types:**
    *   Scatterplot (Constellation diagram)
    *   Histogram (IQ distribution)
    *   RMS (Power over time/samples)
*   **Remote Connection:** Connect to data producers via TCP using the NNG library.
*   **Ingress Filtering:** Filter out noise based on linear magnitude or percentage of samples.
*   **Data Stacking:** Collect and stack samples by timestamp for deeper analysis.
*   **Modern GUI:** Built with [Dear ImGui](https://github.com/ocornut/imgui) and [ImPlot](https://github.com/epezent/implot).

## Prerequisites

To build imscope, you need the following dependencies:

*   **CMake** (>= 3.10)
*   **C++17 Compiler** (GCC, Clang, MSVC)
*   **NNG** (Communication library)
*   **OpenGL** (Graphics API)
*   **GLFW3** (Windowing and input)

Dependencies like `spdlog`, `imgui`, and `implot` are automatically managed via CMake/CPM.

## Build Instructions

```bash
mkdir build
cd build
cmake ..
make -j
```

## Usage

### Running the Application

After building, run the `imscope` executable:

```bash
./imscope
```

1.  **Connect to a Producer:**
    *   In the "Connected consumers" window, enter the address of the producer (default: `tcp://127.0.0.1:5557`).
    *   Click "Connect".
2.  **Select a Scope:**
    *   Once connected, a list of available scopes (data streams) will appear.
    *   Select a scope and click "Open scope window".
3.  **Visualize Data:**
    *   In the scope window, click "Request data" or check "Automatically collect data" for real-time updates.
    *   Choose between "Histogram", "RMS", or "Scatter" plot types.
    *   Adjust settings like ingress filtering or histogram bins as needed.

### Integrating into your Application

To visualize data from your application, you need to use the `imscope` producer API.

1.  **Include the Header:**
    ```c
    #include "imscope_producer.h"
    ```

2.  **Initialize the Producer:**
    Define the scopes (data streams) you want to expose.

    ```c
    imscope_scope_desc_t scopes[] = {
        {"Scope 1", SCOPE_TYPE_IQ_DATA},
        {"Scope 2", SCOPE_TYPE_IQ_DATA}
    };

    // Initialize producer with addresses for data and announce sockets
    imscope_init_producer(
        "tcp://0.0.0.0:5558", // Data address
        "tcp://0.0.0.0:5559", // Announce address
        "My Producer",        // Producer name
        scopes,               // Scope definitions
        2                     // Number of scopes
    );
    ```

3.  **Send Data:**
    Send IQ data (interleaved 16-bit integers usually, based on `uint32_t*` signature implying packed complex IQ).

    ```c
    // Example: Sending data to Scope 1 (ID 0)
    std::vector<uint32_t> iq_data = ...; // Your IQ data
    int frame = 0;
    int slot = 0;
    uint64_t timestamp = ...;

    imscope_return_t ret = imscope_try_send_data(
        iq_data.data(),
        0,              // Scope ID (index in the scopes array)
        iq_data.size(), // Number of samples
        frame,
        slot,
        timestamp
    );
    if (ret != IMSCOPE_SUCCESS) {
        // Handle error or busy state
    }
    ```

## License

This project is licensed under the **MIT License**. This allows for easy integration and modification for both open-source and proprietary applications.

See the [LICENSE](LICENSE) file for the full license text.
