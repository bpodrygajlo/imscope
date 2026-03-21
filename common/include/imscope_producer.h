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

#ifdef __cplusplus
}
#endif

#endif
