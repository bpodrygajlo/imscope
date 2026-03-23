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
find_package(nng CONFIG QUIET)
if(nng_FOUND)
  message(STATUS "Found nng via find_package")
  if(NOT TARGET nng AND TARGET nng::nng)
    add_library(nng ALIAS nng::nng)
  endif()
else()
  CPMAddPackage(
    NAME nng
    GITHUB_REPOSITORY nanomsg/nng
    GIT_TAG v1.9.0
    OPTIONS "NNG_TESTS OFF" "NNG_ENABLE_NNG_COMPAT ON"
  )
endif()

add_library(imscope_internal src/imscope_internal.cpp src/imscope_internal.h)
target_include_directories(imscope_internal PUBLIC src/ include/)
target_link_libraries(imscope_internal PUBLIC nng spdlog)
if(WIN32)
    target_link_libraries(imscope_internal PUBLIC ws2_32 advapi32 shlwapi)
endif()

add_library(imscope_producer src/imscope_producer.cpp)
target_include_directories(imscope_producer PUBLIC include/)
target_link_libraries(imscope_producer PUBLIC nng imscope_internal)

add_library(imscope_consumer src/imscope_consumer.cpp)
target_include_directories(imscope_consumer PUBLIC include/)
target_link_libraries(imscope_consumer PUBLIC nng imscope_internal)

add_library(imscope_tools src/imscope_tools.cpp)
target_include_directories(imscope_tools PUBLIC include/)
target_link_libraries(imscope_tools PUBLIC spdlog)

if(IMSCOPE_BUILD_TESTS)
  enable_testing()
  add_executable(imscope_integration_test test/integration_test.cpp)
  target_link_libraries(imscope_integration_test
      PRIVATE
      imscope_producer
      imscope_consumer
      imscope_internal
      GTest::gtest_main
  )
  include(GoogleTest)
  gtest_discover_tests(imscope_integration_test)
endif()

if(IMSCOPE_BUILD_EXAMPLES)
  add_subdirectory(examples)
endif()
