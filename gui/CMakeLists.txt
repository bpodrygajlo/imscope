cmake_minimum_required(VERSION 3.10)
project(imscope_gui)

find_package(OpenGL REQUIRED)
if(NOT TARGET OpenGL::GL)
    add_library(OpenGL::GL INTERFACE IMPORTED)
    set_target_properties(OpenGL::GL PROPERTIES
        INTERFACE_LINK_LIBRARIES "${OPENGL_LIBRARIES}"
        INTERFACE_INCLUDE_DIRECTORIES "${OPENGL_INCLUDE_DIR}"
    )
endif()

# Fallback for OpenGL::OpenGL if needed by some older parts
if(NOT TARGET OpenGL::OpenGL)
    if(TARGET OpenGL::GL)
        add_library(OpenGL::OpenGL INTERFACE IMPORTED)
        set_target_properties(OpenGL::OpenGL PROPERTIES
            INTERFACE_LINK_LIBRARIES OpenGL::GL
        )
    endif()
endif()

include("../cmake/imgui.cmake")

if(WIN32)
    add_executable(imscope WIN32 src/imscope-gui.cpp)
else()
    add_executable(imscope src/imscope-gui.cpp)
endif()

target_link_libraries(imscope
    PRIVATE
    imscope_consumer
    imscope_producer
    imgui
    implot
    imgui_glfw_backend
    glfw
    imgui_opengl_renderer
    OpenGL::GL
    implot
    imscope_tools
)
