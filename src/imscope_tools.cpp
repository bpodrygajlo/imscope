#include "imscope_tools.h"
#include <spdlog/spdlog.h>
#include <cmath>

void IQSnapshot::preprocess() {
  spdlog::debug("Preprocessing IQ data: size {}", size());
  power.resize(size());
  max_iq = 0;
  max_power = 0;
  nonzero_count = 0;
  for (size_t i = 0; i < size(); i++) {
    int16_t r = real[i];
    int16_t im = imag[i];
    if (abs(r) > max_iq) {
      max_iq = abs(r);
    }
    if (abs(im) > max_iq) {
      max_iq = abs(im);
    }
    float p = r * r + im * im;
    power[i] = p;
    if (p > max_power) {
      max_power = p;
    }
    if (p > 0) {
      nonzero_count++;
    }
  }
}

size_t IQSnapshot::size() {
  return real.size();
}

void IQSnapshot::read_scope_msg(scope_msg_t* msg) {
  spdlog::debug(
      "ImscopeConsumer: Collected IQ data for scope id {} (frame {}, slot "
      "{})",
      scope_id, msg->meta.frame, msg->meta.slot);
  size_t num_samples = msg->data_size / sizeof(uint32_t);
  real.resize(num_samples);
  imag.resize(num_samples);
  int16_t* data = (int16_t*)msg->data;
  for (size_t i = 0; i < num_samples; i += 2) {
    real[i] = data[i];
    imag[i] = data[i + 1];
  }
  preprocess();
}

void VectorSnapshot::preprocess() {
  max = 0;
  nonzero_count = 0;
  for (size_t i = 0; i < v.size(); i++) {
    int16_t value = v[i];
    if (abs(value) > max) {
      max = abs(value);
    }
    if (value != 0) {
      nonzero_count++;
    }
  }
}

void VectorSnapshot::read_scope_msg(scope_msg_t* msg) {
  size_t num_samples = msg->data_size / sizeof(int16_t);
  v.resize(num_samples);
  int16_t* data = (int16_t*)msg->data;
  for (size_t i = 0; i < num_samples; i++) {
    v[i] = data[i];
  }
  preprocess();
}