cmake_minimum_required(VERSION 3.10)
project(imscope_gui)

include("../cmake/imgui.cmake")

add_executable(imscope src/imscope-gui.cpp)
target_link_libraries(imscope
    PRIVATE
    imscope_consumer
    imscope_producer
    imgui
    implot
    imgui_glfw_backend
    glfw
    imgui_opengl_renderer
    OpenGL::OpenGL
    implot
    imscope_tools
)
