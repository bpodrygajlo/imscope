cmake_minimum_required(VERSION 3.10)

# Set build type to Debug by default if not specified
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Debug CACHE STRING "Build type" FORCE)
endif()

# Enable debug symbols
set(CMAKE_CXX_FLAGS_DEBUG "${CMAKE_CXX_FLAGS_DEBUG} -g")
project(imscope_workspace)

# Specify the C++ standard
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED True)

if(MSVC)
  add_compile_options(/W4)
else()
  add_compile_options(-Wall -Wextra -Werror)
endif()

option(IMSCOPE_BUILD_TESTS "Build tests" ON)
option(IMSCOPE_BUILD_EXAMPLES "Build examples" ON)
option(IMSCOPE_BUILD_GUI "Build GUI" ON)

if(NOT CPM_SOURCE_CACHE AND NOT DEFINED ENV{CPM_SOURCE_CACHE})
  if(WIN32)
    set(CPM_SOURCE_CACHE "$ENV{USERPROFILE}/.cache/cpm" CACHE FILEPATH "Directory to download CPM dependencies")
  else()
    set(CPM_SOURCE_CACHE "~/.cache/cpm/" CACHE FILEPATH "Directory to download CPM dependencies")
  endif()
endif()
include("cmake/get_cpm.cmake")

add_subdirectory(common)
if(IMSCOPE_BUILD_GUI)
    add_subdirectory(gui)
endif()
