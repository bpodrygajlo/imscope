#pragma once

#include <cstdint>
#include <vector>
#include "imscope_common.h"

typedef struct MovingAverageTimer {
  uint64_t sum = 0;
  float average = 0;
  float last_update_time = 0;

  void UpdateAverage(float time) {
    if (time > last_update_time + 1) {
      float new_average = sum / (float)((time - last_update_time) / 1000);
      average = 0.95 * average + 0.05 * new_average;
      sum = 0;
    }
  }

  void Add(uint64_t ns) { sum += ns; }
} MovingAverageTimer;

typedef struct {
  // Raw data
  NRmetadata meta;
  int scope_id;
  std::vector<int16_t> real;
  std::vector<int16_t> imag;

  // Derived data
  std::vector<float> power;
  int16_t max_iq;
  float max_power;
  int nonzero_count;
  void preprocess();

 public:
  size_t size();
  void read_scope_msg(scope_msg_t* msg);
} IQSnapshot;

typedef struct {
  // Raw data
  NRmetadata meta;
  int scope_id;
  std::vector<int16_t> v;

  // Derived data
  int16_t max;
  int nonzero_count;
  void preprocess();

 public:
  size_t size() const { return v.size(); }

  void read_scope_msg(scope_msg_t* msg);
} VectorSnapshot;
