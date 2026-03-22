/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#ifndef IMSCOPE_COMMON_H
#define IMSCOPE_COMMON_H

#ifdef __cplusplus
#include <cstddef>
#include <cstdint>
#else
#include <stddef.h>
#include <stdint.h>
#endif

#define MAX_OFFSETS 14
#define MAX_SCOPE_NAME_LEN 64
#define ANNOUNCE_MSG_ID 0xABCDEF01

typedef struct {
  uint32_t frame;
  uint32_t slot;
  uint64_t timestamp;
} NRmetadata;

typedef enum {
  IMSCOPE_SUCCESS = 0,
  IMSCOPE_ERROR_NOT_INITIALIZED = -1,
  IMSCOPE_ERROR_INVALID_ID = -2,
  IMSCOPE_ERROR_BUSY = -3,
} imscope_return_t;

typedef enum {
  SCOPE_TYPE_REAL = 0,
  SCOPE_TYPE_IQ_DATA = 1,
} scope_type_t;

typedef struct {
  NRmetadata meta;
  uint64_t time_taken_in_ns;
  int id;
  size_t data_size;
  char data[1];
} scope_msg_t;

typedef struct {
  int id;
  int credits;
} control_msg_t;

typedef struct {
  char name[MAX_SCOPE_NAME_LEN];
  scope_type_t type;
} imscope_scope_config_t;

typedef struct {
  char data_address[128];
  char control_address[128];
  char name[128];
  int num_scopes;
  imscope_scope_config_t scopes[1];
} announce_response_t;

typedef struct {
  uint32_t magic;
} announce_request_t;

#endif
