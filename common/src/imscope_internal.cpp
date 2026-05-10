/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#include <nng/nng.h>
#include <nng/protocol/pipeline0/pull.h>
#include <nng/protocol/pipeline0/push.h>
#include <nng/protocol/reqrep0/rep.h>
#include <nng/protocol/reqrep0/req.h>
#include <cstdlib>
#include <iostream>

#include <cstdarg>

#include "imscope_common.h"
#include "imscope_internal.h"

void FatalError(const char* format, ...) {
  std::cerr << "Fatal error: ";
  va_list args;
  va_start(args, format);
  vfprintf(stderr, format, args);
  va_end(args);
  std::cerr << std::endl;
  std::abort();
}

nng_socket create_nng_push_socket(const char* address) {
  nng_socket socket;
  int rv = nng_push0_open(&socket);
  if (rv != 0) {
    FatalError("nng_push0_open failed: %s", nng_strerror(rv));
  }

  rv = nng_listen(socket, address, NULL, 0);
  if (rv != 0) {
    FatalError("nng_listen failed address %s: %s", address, nng_strerror(rv));
  }
  return socket;
}

nng_socket create_nng_pull_socket(const char* address) {
  nng_socket socket;
  int rv = nng_pull0_open(&socket);
  if (rv != 0) {
    FatalError("nng_pull0_open failed: %s", nng_strerror(rv));
  }

  rv = nng_dial(socket, address, NULL, NNG_FLAG_NONBLOCK);
  if (rv != 0) {
    FatalError("nng_dial failed: %s", nng_strerror(rv));
  }
  return socket;
}

nng_socket create_nng_req_socket(const char* address) {
  nng_socket socket;
  int rv = nng_req0_open(&socket);
  if (rv != 0) {
    FatalError("nng_req0_open failed: %s", nng_strerror(rv));
  }

  rv = nng_dial(socket, address, NULL, NNG_FLAG_NONBLOCK);
  if (rv != 0) {
    FatalError("nng_dial failed: %s", nng_strerror(rv));
  }
  return socket;
}

nng_socket create_nng_rep_socket(const char* address) {
  nng_socket socket;
  int rv = nng_rep0_open(&socket);
  if (rv != 0) {
    FatalError("nng_rep0_open failed: %s", nng_strerror(rv));
  }

  rv = nng_listen(socket, address, NULL, 0);
  if (rv != 0) {
    FatalError("nng_listen failed address %s: %s", address, nng_strerror(rv));
  }
  return socket;
}

void print_announce_response(announce_response_t* msg) {
  std::cout << "" << msg->name << std::endl;
  std::cout << "Data address: " << msg->data_address << std::endl;
  std::cout << "---------------------------------" << std::endl;
  std::cout << "Number of scopes: " << msg->num_scopes << std::endl;
  for (int i = 0; i < msg->num_scopes; i++) {
    std::cout << "  Scope " << i << ": " << msg->scopes[i].name << " Type: "
              << (msg->scopes[i].type == SCOPE_TYPE_REAL ? "REAL" : "IQ_DATA")
              << std::endl;
  }
}
