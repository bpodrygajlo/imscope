cmake_minimum_required(VERSION 3.10)

# Add all example executables
add_executable(simple simple.cpp)
target_link_libraries(simple PRIVATE imscope_producer spdlog)

add_executable(multi_scope multi_scope.cpp)
target_link_libraries(multi_scope PRIVATE imscope_producer spdlog)

add_executable(sine_wave sine_wave.cpp)
target_link_libraries(sine_wave PRIVATE imscope_producer spdlog)

add_executable(zero_copy zero_copy.cpp)
target_link_libraries(zero_copy PRIVATE imscope_producer imscope_internal)
