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
#define MAX_GROUP_NAME_LEN 64
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
  IMSCOPE_ERROR_INTERNAL = -4,
} imscope_return_t;

typedef enum {
  SCOPE_TYPE_REAL = 0,
  SCOPE_TYPE_IQ_DATA = 1,
  SCOPE_TYPE_INT32 = 2,
  SCOPE_TYPE_FLOAT = 3,
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
  char group[MAX_GROUP_NAME_LEN];
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

#define SCOPE_REQ_MSG_ID 0xABCDEF02

typedef struct {
  uint32_t magic;
  int32_t scope_id;
} scope_request_t;

typedef enum {
  SETTING_TYPE_BOOL = 0,
  SETTING_TYPE_INT32 = 1,
  SETTING_TYPE_FLOAT = 2
} setting_type_t;

typedef struct {
  char name[64];
  setting_type_t type;

  union {
    uint8_t bval;
    int32_t ival;
    float fval;
  } value;
} imscope_setting_t;

#define SETTING_REQ_GET_ALL 0xABCDEF10
#define SETTING_REQ_SET 0xABCDEF11
#define SETTING_REP_GET_ALL 0xABCDEF20
#define SETTING_REP_SET 0xABCDEF21

typedef struct {
  uint32_t magic;
  char name[64];
  setting_type_t type;

  union {
    uint8_t bval;
    int32_t ival;
    float fval;
  } value;
} setting_request_t;

typedef struct {
  uint32_t magic;
  int32_t status;
  int32_t num_settings;
  imscope_setting_t settings[1];
} setting_response_t;

#endif
