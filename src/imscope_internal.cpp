#include <nanomsg/nn.h>
#include <nanomsg/pipeline.h>
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

int create_nn_push_socket(const char* address) {
  int socket = nn_socket(AF_SP, NN_PUSH);
  if (socket < 0) {
    FatalError("nn_socket failed: %s", nn_strerror(nn_errno()));
  }

  int ret = nn_bind(socket, address);
  if (ret < 0) {
    FatalError("nn_bind failed address %s: %s", address,
               nn_strerror(nn_errno()));
  }
  return socket;
}

int create_nn_pull_socket(const char* address) {
  int socket = nn_socket(AF_SP, NN_PULL);
  if (socket < 0) {
    FatalError("nn_socket failed: %s", nn_strerror(nn_errno()));
  }

  int ret = nn_connect(socket, address);
  if (ret < 0) {
    FatalError("nn_connect failed: %s", nn_strerror(nn_errno()));
  }
  return socket;
}

void print_announce_response(announce_response_t* msg) {
  std::cout << "" << msg->name << std::endl;
  std::cout << "Data address: " << msg->data_address << std::endl;
  std::cout << "Control address: " << msg->control_address << std::endl;
  std::cout << "---------------------------------" << std::endl;
  std::cout << "Number of scopes: " << msg->num_scopes << std::endl;
  for (int i = 0; i < msg->num_scopes; i++) {
    std::cout << "  Scope " << i << ": " << msg->scopes[i].name << " Type: "
              << (msg->scopes[i].type == SCOPE_TYPE_REAL ? "REAL" : "IQ_DATA")
              << std::endl;
  }
}