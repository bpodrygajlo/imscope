/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#ifndef IMSCOPE_INTERFACE_PRODUCER_H
#define IMSCOPE_INTERFACE_PRODUCER_H

#include "imscope_common.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
  const char* name;
  scope_type_t type;
} imscope_scope_desc_t;

imscope_return_t imscope_init_producer(const char* data_address,
                                       const char* announce_address,
                                       const char* name,
                                       imscope_scope_desc_t* scopes,
                                       size_t num_scopes);

/**
 * @brief Register a scope dynamically.
 * @param name Scope name
 * @param type Scope type
 * @return Assigned scope ID, or IMSCOPE_ERROR_NOT_INITIALIZED / IMSCOPE_ERROR_INTERNAL
 */
int imscope_register_scope(const char* name, scope_type_t type);

/**
 * @brief Send data to a scope by name.
 *        Automatically registers the scope if it doesn't exist.
 * @return IMSCOPE_SUCCESS on success
 */
imscope_return_t imscope_try_send_data_by_name(uint32_t* data, const char* name,
                                               scope_type_t type,
                                               size_t num_samples, int frame,
                                               int slot, uint64_t timestamp);

/**
 * @brief Acquire a buffer for zero-copy send by name.
 *        Automatically registers the scope if it doesn't exist.
 * @return Pointer to the data buffer, or NULL
 */
void* imscope_acquire_send_buffer_by_name(const char* name, scope_type_t type,
                                          size_t num_samples);

/**
 * @brief Commit and send an acquired buffer by name.
 * @return IMSCOPE_SUCCESS on success
 */
imscope_return_t imscope_commit_send_buffer_by_name(const char* name,
                                                    size_t num_samples,
                                                    int frame, int slot,
                                                    uint64_t timestamp);

/**
 * @brief Send data to a scope.
 * @return IMSCOPE_SUCCESS on success, IMSCOPE_ERROR_INVALID_ID/NOT_INITIALIZED on error, IMSCOPE_ERROR_BUSY if scope is busy (waiting for REP)
 */
imscope_return_t imscope_try_send_data(uint32_t* data, int id,
                                       size_t num_samples, int frame, int slot,
                                       uint64_t timestamp);

/**
 * @brief Acquire a buffer for zero-copy send.
 * @param id Scope ID
 * @param num_samples Number of samples to allocate
 * @return Pointer to the data buffer, or NULL if busy/busy/error
 */
void* imscope_acquire_send_buffer(int id, size_t num_samples);

/**
 * @brief Commit and send an acquired buffer.
 * @return IMSCOPE_SUCCESS on success
 */
imscope_return_t imscope_commit_send_buffer(int id, size_t num_samples,
                                            int frame, int slot,
                                            uint64_t timestamp);

/**
 * @brief Cleanup the producer instance and resources.
 */
void imscope_cleanup_producer();

/**
 * @brief Push a single int32_t value to a scope by ID.
 *        Collects values and sends them when threshold is met or timeout expires.
 */
imscope_return_t imscope_try_send_int32(int32_t val, int id);

/**
 * @brief Push a single float value to a scope by ID.
 *        Collects values and sends them when threshold is met or timeout expires.
 */
imscope_return_t imscope_try_send_float(float val, int id);

/**
 * @brief Push a single int32_t value to a scope by name.
 *        Automatically registers the scope as SCOPE_TYPE_INT32 if it doesn't exist.
 */
imscope_return_t imscope_try_send_int32_by_name(int32_t val, const char* name);

/**
 * @brief Push a single float value to a scope by name.
 *        Automatically registers the scope as SCOPE_TYPE_FLOAT if it doesn't exist.
 */
imscope_return_t imscope_try_send_float_by_name(float val, const char* name);

/**
 * @brief Push a single int32_t value to a named scope within a group.
 *        Registers the scope as SCOPE_TYPE_INT32 under the given group if it doesn't exist.
 */
imscope_return_t imscope_try_send_int32_by_group(int32_t val, const char* name,
                                                 const char* group);

/**
 * @brief Push a single float value to a named scope within a group.
 *        Registers the scope as SCOPE_TYPE_FLOAT under the given group if it doesn't exist.
 */
imscope_return_t imscope_try_send_float_by_group(float val, const char* name,
                                                 const char* group);

#ifdef __cplusplus
}
#endif

#endif
