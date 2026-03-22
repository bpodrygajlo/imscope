# imscope - AI Agent Context (Jules/Gemini)

`imscope` is a real-time signal visualization tool designed for high-throughput IQ data (e.g., 5G stacks). It utilizes **NNG** for communication and **Dear ImGui** with **ImPlot** for the graphical interface.

## Tech Stack
- **Language:** C++17
- **Communication:** NNG
- **GUI:** Dear ImGui, ImPlot, OpenGL3, GLFW3
- **Logging:** spdlog

## Project Structure & File Descriptions

### Core Library (`common/`)
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

### Graphical Interface (`gui/`)
- [gui/CMakeLists.txt](gui/CMakeLists.txt): Build system for the GUI component.
- [gui/src/imscope-gui.cpp](gui/src/imscope-gui.cpp): The application's entry point. Initializes the GUI (GLFW/OpenGL/ImGui), manages the connection UI for consumers, and handles the lifecycle of scope windows.

### Root Files
- [CMakeLists.txt](CMakeLists.txt): Root build configuration, coordinates subdirectories.
- [README.md](README.md): General documentation, installation, and usage guide.
- [.pre-commit-config.yaml](.pre-commit-config.yaml): Code quality checks.
