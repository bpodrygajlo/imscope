/*
 * Licensed to the OpenAirInterface (OAI) Software Alliance under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The OpenAirInterface Software Alliance licenses this file to You under
 * the OAI Public License, Version 1.1  (the "License"); you may not use this
 *file except in compliance with the License. You may obtain a copy of the
 *License at
 *
 *      http://www.openairinterface.org/?page_id=698
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *-------------------------------------------------------------------------------
 * For more information about the OpenAirInterface (OAI) Software Alliance:
 *      contact@openairinterface.org
 */

#include <stdio.h>
#include <cstdint>
#include <optional>
#include "imgui.h"
#include "imgui_impl_glfw.h"
#include "imgui_impl_opengl3.h"
#include "imscope_tools.h"
#include "src/imscope_tools.h"
#define GL_SILENCE_DEPRECATION
#if defined(IMGUI_IMPL_OPENGL_ES2)
#include <GLES2/gl2.h>
#endif
#include <GLFW/glfw3.h>  // Will drag system OpenGL headers

#include <spdlog/spdlog.h>
#include <cstdlib>
#include <map>
#include <sstream>
#include <vector>
#include "implot.h"
#include "imscope_consumer.h"

static void glfw_error_callback(int error, const char* description) {
  fprintf(stderr, "GLFW Error %d: %s\n", error, description);
}

bool show_popup();

typedef struct {
  std::string name;
  ImscopeConsumer* consumer;
} consumer_entry_t;

static std::vector<consumer_entry_t> consumers;

typedef struct {
  int num_bins = 50;
  bool autoscale = true;
} IQHistogram;

typedef struct {
  int scope_id;
  ImscopeConsumer* consumer;
  IQSnapshot iq_data;
  int plot_type;
  IQHistogram histogram_settings;
  bool auto_collect = false;

  bool filter_enabled = false;
  float noise_cutoff_linear = 0.0f;
  float noise_cutoff_percentage = 50.0f;
  int handle = 0;
  bool collecting = false;
} scope_window_t;

static std::map<std::pair<ImscopeConsumer*, int>, scope_window_t> scope_windows;

void show_scatterplot(const char* label, IQSnapshot& iq_data);
void show_histogram(const char* label, IQSnapshot& iq_data,
                    IQHistogram& settings);
void show_rms(const char* label, IQSnapshot& iq_data);

void show_metadata(const NRmetadata& meta) {
  ImGui::BeginGroup();
  std::stringstream ss;
  if (meta.slot != -1) {
    ss << " slot: " << meta.slot;
  }
  if (meta.frame != -1) {
    ss << " frame: " << meta.frame;
  }
  if (!ss.str().empty()) {
    ImGui::Text("Data for %s", ss.str().c_str());
  }
  ImGui::EndGroup();
}

void show_scope_window(scope_window_t& scope_window) {
  ImGui::Begin((scope_window.consumer->get_name() + " - Scope " +
                scope_window.consumer->get_scope_name(scope_window.scope_id))
                   .c_str());
  if (ImGui::Checkbox("Automatically collect data",
                      &scope_window.auto_collect)) {
    if (scope_window.auto_collect) {
      scope_window.consumer->request_scope_data(scope_window.scope_id, 1);
    }
  }

  bool fit = false;
  ImGui::Checkbox("Enable ingress filtering", &scope_window.filter_enabled);
  if (ImGui::IsItemHovered()) {
    ImGui::SetTooltip("Filter out scope messages with too much noise before plotting");
  }
  if (scope_window.filter_enabled) {
    if (ImGui::SliderFloat("Noise cutoff (linear)", &scope_window.noise_cutoff_linear, 0.0f, INT16_MAX)) {
      fit = true;
    }
    if (ImGui::IsItemHovered()) {
      ImGui::SetTooltip("Samples with magnitude below this value are considered noise");
    }
    ImGui::SliderFloat("Noise cutoff percentage", &scope_window.noise_cutoff_percentage, 0.0f, 100.0f);
    if (ImGui::IsItemHovered()) {
      ImGui::SetTooltip("What percentage of samples can be noise before rejecting the entire scope message");
    }
  }
  ImGui::Checkbox("Enable collecting by timestamp", &scope_window.collecting);
  if (ImGui::IsItemHovered()) {
    ImGui::SetTooltip("Collect incoming data by scope message timestamp. This will stack samples in order of timestamp.");
  }
  if (scope_window.collecting) {
    ImGui::SliderInt("Size of stacked data", (int*)&scope_window.iq_data.max_stacked_size, 1000, 100000);
  }

  if (!scope_window.auto_collect) {
    if (ImGui::Button("Request data")) {
      scope_window.consumer->request_scope_data(scope_window.scope_id, 1);
    }
  }
  auto msg =
      scope_window.consumer->try_collect_scope_msg(scope_window.scope_id, scope_window.handle);
  if (msg.get() != nullptr) {
    if (scope_window.filter_enabled) {
      if (scope_window.iq_data.read_scope_msg(static_cast<scope_msg_t*>(msg.get()), scope_window.noise_cutoff_linear, scope_window.noise_cutoff_percentage)) {
        fit = true;
      }
    }
    else {
      scope_window.iq_data.read_scope_msg(static_cast<scope_msg_t*>(msg.get()), scope_window.collecting);
      ImPlot::SetNextAxesToFit();
    }
    if (scope_window.auto_collect) {
      scope_window.consumer->request_scope_data(scope_window.scope_id, 1);
    }
  }

  const char* items[] = {"Histogram", "RMS", "Scatter"};
  ImGui::Combo("Select plot type", &scope_window.plot_type, items,
               IM_ARRAYSIZE(items));

  switch (scope_window.plot_type) {
    case 0:
      show_histogram("IQ Histogram", scope_window.iq_data,
                     scope_window.histogram_settings);
      break;
    case 1:
      show_rms("IQ RMS", scope_window.iq_data);
      break;
    case 2:
      show_scatterplot("IQ Scatterplot", scope_window.iq_data);
      break;
  }

  show_metadata(scope_window.iq_data.meta);
  ImGui::End();
}

void show_scatterplot(const char* label, IQSnapshot& iq_data) {
  float x = ImGui::CalcItemWidth();
  if (ImPlot::BeginPlot(label, {x, x})) {
    int points_drawn = 0;
    ImPlot::SetNextMarkerStyle(ImPlotMarker_Circle, 1, IMPLOT_AUTO_COL, 1);
    while (points_drawn < iq_data.size()) {
      // Limit the amount of data plotted with PlotScatter call (issue with vertices/draw call)
      int points_to_draw = std::min(iq_data.size() - points_drawn, 16000UL);
      ImPlot::PlotScatter(label, iq_data.real.data() + points_drawn,
                          iq_data.imag.data() + points_drawn, points_to_draw);
      points_drawn += points_to_draw;
    }
    ImPlot::EndPlot();
  }
}

void show_histogram(const char* label, IQSnapshot& iq_data,
                    IQHistogram& settings) {
  ImGui::SliderInt("Number of bins", &settings.num_bins, 10, 500);
  ImGui::Checkbox("Autoscale", &settings.autoscale);
  float range = iq_data.max_iq * 1.2;
  auto plot_range = settings.autoscale
                        ? ImPlotRect(-range, range, -range, range)
                        : ImPlotRect();
  float x = ImGui::CalcItemWidth();
  if (ImPlot::BeginPlot(label, {x, x})) {
    ImPlot::PlotHistogram2D(label, iq_data.real.data(), iq_data.imag.data(),
                            iq_data.real.size(), settings.num_bins,
                            settings.num_bins, plot_range);
    ImPlot::EndPlot();
  }
}

void show_rms(const char* label, IQSnapshot& iq_data) {
  if (ImPlot::BeginPlot(label)) {
    ImPlot::PlotLine(label, iq_data.power.data(), iq_data.power.size());
    ImPlot::EndPlot();
  }
}

void draw_menu_bar(bool& show_imgui_demo_window, bool& show_implot_demo_window,
                   bool& reset_ini_settings, bool& close_window) {
  if (ImGui::BeginMainMenuBar()) {
    if (ImGui::BeginMenu("File")) {
      if (ImGui::MenuItem("Close scope")) {
        close_window = true;
      }
      ImGui::EndMenu();
    }
    if (ImGui::BeginMenu("Options")) {
      ImGui::Checkbox("Show imgui demo window", &show_imgui_demo_window);
      ImGui::Checkbox("Show implot demo window", &show_implot_demo_window);
      ImGui::EndMenu();
    }
    if (ImGui::BeginMenu("Layout")) {
      if (ImGui::MenuItem("Reset")) {
        reset_ini_settings = true;
      }
      ImGui::EndMenu();
    }
    ImGui::EndMainMenuBar();
  }
}

const char* get_names(void* arg, int idx) {
  const char* name = consumers[idx].name.c_str();
  return name;
}

const char* get_scope_names(void* arg, int idx) {
  ImscopeConsumer* entry = static_cast<ImscopeConsumer*>(arg);
  const char* name = entry->get_scope_name(idx);
  return name;
}

void show_consumers() {
  ImGui::Begin("Connected consumers");
  static int selected = -1;
  ImGui::ListBox("Connected consumers", &selected, get_names, NULL,
                 consumers.size(), 4);
  static int selected_scope = -1;
  if (selected >= 0 && selected < (int)consumers.size())
    ImGui::ListBox("Scopes", &selected_scope, get_scope_names,
                   consumers[selected].consumer,
                   consumers[selected].consumer->get_num_scopes(), 4);
  if (selected_scope >= 0 && selected >= 0 &&
      selected < (int)consumers.size()) {
    auto selected_pair =
        std::make_pair(consumers[selected].consumer, selected_scope);
    if (scope_windows.count(selected_pair) == 0) {
      if (ImGui::Button("Open scope window")) {
        scope_windows[selected_pair] = {
            selected_scope, consumers[selected].consumer, IQSnapshot()};
      }
    } else {
      if (ImGui::Button("Close scope window")) {
        scope_windows.erase(selected_pair);
      }
    }
  }
  ImGui::Separator();
  ImGui::Text("Add consumer");
  static char buf[128] = "tcp://127.0.0.1:5557";
  ImGui::InputText("Consumer address", buf, IM_ARRAYSIZE(buf));
  if (ImGui::Button("Connect")) {
    ImscopeConsumer* consumer = ImscopeConsumer::connect(buf);
    if (consumer) {
      consumers.push_back(
          {.name = consumer->get_name() + "@" + std::string(buf),
           .consumer = consumer});
    } else {
      printf("Failed to connect to consumer at address %s\n", buf);
    }
  }
  ImGui::End();
}

void imscope_thread(void) {
  glfwSetErrorCallback(glfw_error_callback);
  if (!glfwInit())
    return;

  // Decide GL+GLSL versions
#if defined(IMGUI_IMPL_OPENGL_ES2)
  // GL ES 2.0 + GLSL 100
  const char* glsl_version = "#version 100";
  glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 2);
  glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 0);
  glfwWindowHint(GLFW_CLIENT_API, GLFW_OPENGL_ES_API);
#elif defined(__APPLE__)
  // GL 3.2 + GLSL 150
  const char* glsl_version = "#version 150";
  glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
  glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 2);
  glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);  // 3.2+ only
  glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GL_TRUE);  // Required on Mac
#else
  // GL 3.0 + GLSL 130
  const char* glsl_version = "#version 130";
  glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
  glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 0);
  // glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);  // 3.2+
  // only glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GL_TRUE); // 3.0+ only
#endif

  // Create window with graphics context
  GLFWwindow* window = glfwCreateWindow(1280, 720, "imscope", nullptr, nullptr);
  if (window == nullptr)
    return;
  glfwMakeContextCurrent(window);
  glfwSwapInterval(1);  // For frame capping

  // Setup Dear ImGui context
  IMGUI_CHECKVERSION();
  ImGui::CreateContext();
  ImPlot::CreateContext();
  ImGuiIO& io = ImGui::GetIO();
  (void)io;
  io.ConfigFlags |=
      ImGuiConfigFlags_NavEnableKeyboard;  // Enable Keyboard Controls
  io.ConfigFlags |= ImGuiConfigFlags_DockingEnable;

  // Setup Dear ImGui style
  ImGui::StyleColorsDark();
  // ImGui::StyleColorsLight();

  // Setup Platform/Renderer backends
  ImGui_ImplGlfw_InitForOpenGL(window, true);
#ifdef __EMSCRIPTEN__
  ImGui_ImplGlfw_InstallEmscriptenCallbacks(window, "#canvas");
#endif
  ImGui_ImplOpenGL3_Init(glsl_version);

  // Our state
  ImVec4 clear_color = ImVec4(0.45f, 0.55f, 0.60f, 1.00f);

  bool close_window = false;
  while (!glfwWindowShouldClose(window) && close_window == false) {
    // Poll and handle events (inputs, window resize, etc.)
    // You can read the io.WantCaptureMouse, io.WantCaptureKeyboard flags to
    // tell if dear imgui wants to use your inputs.
    // - When io.WantCaptureMouse is true, do not dispatch mouse input data to
    // your main application, or clear/overwrite your copy of the mouse data.
    // - When io.WantCaptureKeyboard is true, do not dispatch keyboard input
    // data to your main application, or clear/overwrite your copy of the
    // keyboard data. Generally you may always pass all inputs to dear imgui,
    // and hide them from your application based on those two flags.
    glfwPollEvents();

    // Start the Dear ImGui frame
    ImGui_ImplOpenGL3_NewFrame();
    ImGui_ImplGlfw_NewFrame();

    static bool reset_ini_settings = false;
    if (reset_ini_settings) {
      ImGui::LoadIniSettingsFromDisk("imscope-init.ini");
      reset_ini_settings = false;
    }
    ImGui::NewFrame();

    int display_w, display_h;
    glfwGetFramebufferSize(window, &display_w, &display_h);

    static float t = 0;
    static bool show_imgui_demo_window = false;
    static bool show_implot_demo_window = false;
    ImGui::DockSpaceOverViewport();
    draw_menu_bar(show_imgui_demo_window, show_implot_demo_window,
                  reset_ini_settings, close_window);
    std::string address;

    show_consumers();
    for (auto& scope_window : scope_windows) {
      show_scope_window(scope_window.second);
    }

    ImGui::Begin("Global scope settings");
    ImGui::ShowStyleSelector("ImGui Style");
    ImPlot::ShowStyleSelector("ImPlot Style");
    ImPlot::ShowColormapSelector("ImPlot Colormap");
    ImGui::End();

    // For reference
    if (show_implot_demo_window)
      ImPlot::ShowDemoWindow();
    if (show_imgui_demo_window)
      ImGui::ShowDemoWindow();

    // Rendering
    ImGui::Render();
    glViewport(0, 0, display_w, display_h);
    glClearColor(clear_color.x * clear_color.w, clear_color.y * clear_color.w,
                 clear_color.z * clear_color.w, clear_color.w);
    glClear(GL_COLOR_BUFFER_BIT);
    ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());

    glfwSwapBuffers(window);
  }

  // Cleanup
  ImGui_ImplOpenGL3_Shutdown();
  ImGui_ImplGlfw_Shutdown();
  ImGui::DestroyContext();

  glfwDestroyWindow(window);
  glfwTerminate();
}

int main(int argc, char** argv) {
  spdlog::set_level(spdlog::level::debug);
  imscope_thread();
}