/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <nng/nng.h>
#include "imscope_common.h"

void FatalError(const char* format, ...);
nng_socket create_nng_push_socket(const char* address);
nng_socket create_nng_pull_socket(const char* address);
nng_socket create_nng_req_socket(const char* address);
nng_socket create_nng_rep_socket(const char* address);
void print_announce_response(announce_response_t* msg);
