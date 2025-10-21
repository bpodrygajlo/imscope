#pragma once

#include "imscope_common.h"

void FatalError(const char* format, ...);
int create_nn_push_socket(const char* address);
int create_nn_pull_socket(const char* address);
void print_announce_response(announce_response_t* msg);