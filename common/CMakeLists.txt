cmake_minimum_required(VERSION 3.10)
project(imscope_common)

include("../cmake/get_cpm.cmake")
CPMAddPackage("gh:gabime/spdlog#v1.16.0")
CPMAddPackage(
  NAME googletest
  GITHUB_REPOSITORY google/googletest
  VERSION 1.15.2
  OPTIONS "INSTALL_GTEST OFF" "gtest_force_shared_crt ON"
)
# zenoh-c is the official C library built from the Rust `zenoh` crate (hence
# the `cargo`/`rustc` requirement, already satisfied by this repo's Rust
# side); zenoh-cpp is a header-only C++ wrapper over it. zenoh-c must be
# added first: zenoh-cpp's own CMakeLists.txt does
# `if(NOT TARGET zenohc::lib) find_package(zenohc REQUIRED) endif()`, so
# adding zenoh-c first (which defines that target as an IMPORTED GLOBAL
# target) makes zenoh-cpp pick it up directly instead of calling
# find_package(). Pinned to 1.10.1 to match the Rust side's `zenoh = "1.10.1"`.
# Pre-set BUILD_SHARED_LIBS to OFF in cache so zenoh-c's declare_cache_var
# doesn't default it to TRUE (which would otherwise turn all unadorned
# add_library() calls in parent projects like OAI into SHARED libraries).
set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build static libraries" FORCE)

CPMAddPackage(
  NAME zenohc
  GITHUB_REPOSITORY eclipse-zenoh/zenoh-c
  GIT_TAG 1.10.1
  OPTIONS "BUILD_SHARED_LIBS OFF" "ZENOHC_BUILD_WITH_UNSTABLE_API OFF"
          "ZENOHC_BUILD_WITH_SHARED_MEMORY OFF"
)
# zenoh-c's own CMake generates a fresh Cargo.toml for its Rust build under
# our top-level CMAKE_BINARY_DIR (a subdirectory of this repo). This repo's
# root Cargo.toml declares an explicit-member Cargo workspace, so cargo
# treats that generated manifest as an orphaned, undeclared member of the
# outer workspace and refuses to build it ("current package believes it's in
# a workspace when it's not"). Since the root Cargo.toml is off-limits here
# (owned by the Rust-side migration stages), give zenoh-c's generated
# manifests their own empty `[workspace]` table instead, which makes cargo
# treat each as a standalone workspace root and stops it searching upward.
foreach(_zenohc_cargo_toml
        "${CMAKE_BINARY_DIR}/_deps/zenohc-build/debug/Cargo.toml"
        "${CMAKE_BINARY_DIR}/_deps/zenohc-build/release/Cargo.toml")
  if(EXISTS "${_zenohc_cargo_toml}")
    file(READ "${_zenohc_cargo_toml}" _zenohc_cargo_toml_contents)
    if(NOT _zenohc_cargo_toml_contents MATCHES "\\[workspace\\]")
      file(APPEND "${_zenohc_cargo_toml}" "\n[workspace]\n")
    endif()
  endif()
endforeach()

CPMAddPackage(
  NAME zenohcpp
  GITHUB_REPOSITORY eclipse-zenoh/zenoh-cpp
  GIT_TAG 1.10.1
  OPTIONS "ZENOHCXX_ZENOHC ON" "ZENOHCXX_ZENOHPICO OFF"
          "ZENOHCXX_ENABLE_TESTS OFF" "ZENOHCXX_ENABLE_EXAMPLES OFF"
)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build static libraries" FORCE)

# Explicitly STATIC (matching this trio's original implicit behavior, before
# this file ever set BUILD_SHARED_LIBS/OPTIONS for anything): zenoh-c's own
# CMakeLists.txt declares a project-wide `BUILD_SHARED_LIBS` cache variable
# (repurposed as "alias zenohc::lib to zenohc::shared vs zenohc::static") the
# first time anything in this configure run touches it. On a from-scratch
# configure that cache entry defaults to TRUE, which -- since these
# `add_library()` calls run after zenoh-c/zenoh-cpp are added -- would
# otherwise make them build as shared libraries themselves and then fail to
# link against spdlog's static, non-PIC archive. Being explicit here avoids
# depending on that ambient, easily-perturbed default.
add_library(imscope_internal STATIC src/imscope_internal.cpp src/imscope_internal.h)
target_include_directories(imscope_internal PUBLIC src/ include/)
target_link_libraries(imscope_internal PUBLIC zenohcxx::zenohc spdlog)
set_target_properties(imscope_internal PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(imscope_producer STATIC src/imscope_producer.cpp)
target_include_directories(imscope_producer PUBLIC include/)
target_link_libraries(imscope_producer PUBLIC zenohcxx::zenohc imscope_internal)
set_target_properties(imscope_producer PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(imscope_consumer STATIC src/imscope_consumer.cpp)
target_include_directories(imscope_consumer PUBLIC include/)
target_link_libraries(imscope_consumer PUBLIC zenohcxx::zenohc imscope_internal)
set_target_properties(imscope_consumer PROPERTIES POSITION_INDEPENDENT_CODE ON)

if(IMSCOPE_BUILD_TESTS)
  add_subdirectory(test)
endif()

if(IMSCOPE_BUILD_EXAMPLES)
  add_subdirectory(examples)
endif()
