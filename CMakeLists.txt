cmake_minimum_required(VERSION 3.10)

# Set build type to Debug by default if not specified
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Debug CACHE STRING "Build type" FORCE)
endif()

# Enable debug symbols
set(CMAKE_CXX_FLAGS_DEBUG "${CMAKE_CXX_FLAGS_DEBUG} -g")
project(imscope)

# Specify the C++ standard
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED True)

if(NOT DEFINED ENV{CPM_SOURCE_CACHE})
  set(CPM_SOURCE_CACHE ~/.cache/cpm/ CACHE FILEPATH "Directory to download CPM dependencies")
endif()
include("cmake/get_cpm.cmake")
include("cmake/imgui.cmake")
CPMAddPackage("gh:gabime/spdlog#v1.16.0")

add_library(imscope_internal src/imscope_internal.cpp)
target_include_directories(imscope_internal PUBLIC src/ include/)
target_link_libraries(imscope_internal PUBLIC nanomsg spdlog)

add_library(imscope_producer src/imscope_producer.cpp)
target_include_directories(imscope_producer PUBLIC include/)
target_link_libraries(imscope_producer PUBLIC nanomsg imscope_internal)

add_library(imscope_consumer src/imscope_consumer.cpp)
target_include_directories(imscope_consumer PUBLIC include/)
target_link_libraries(imscope_consumer PUBLIC nanomsg imscope_internal)

add_library(imscope_tools src/imscope_tools.cpp)
target_include_directories(imscope_tools PUBLIC include/)
target_link_libraries(imscope_tools PUBLIC spdlog)

add_executable(imscope main.cpp)
target_link_libraries(imscope imscope_consumer imscope_producer imgui implot imgui_glfw_backend glfw imgui_opengl_renderer OpenGL::OpenGL implot imscope_tools)

add_executable(imscope_test test/main.cpp)
target_link_libraries(imscope_test PRIVATE imscope_producer)