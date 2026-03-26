add_executable(integration_test integration_test.cpp)
target_link_libraries(integration_test
    PRIVATE
    imscope_producer
    imscope_consumer
    imscope_internal
    GTest::gtest_main
)
add_test(
    NAME integration_test
    COMMAND integration_test
    WORKING_DIRECTORY ${CMAKE_BINARY_DIR}
)
