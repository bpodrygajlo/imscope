#include "imscope_consumer.h"
#include <nanomsg/nn.h>
#include <nanomsg/pipeline.h>
#include <nanomsg/reqrep.h>
#include <spdlog/spdlog.h>
#include <cstddef>
#include <mutex>
#include <queue>
#include <thread>
#include <vector>
#include "imscope_common.h"
#include "imscope_internal.h"


void ImscopeConsumer::start_consumer_thread() {
  std::thread([this]() {
    try {
      pthread_setname_np(pthread_self(), "imscope_consumer");
      const char* data_address = this->data_address.c_str();
      int socket = create_nn_pull_socket(data_address);

      while (1) {
        void* msg_buf = NULL;
        int bytes = nn_recv(socket, &msg_buf, NN_MSG, 0);
        scope_msg_t* msg = (scope_msg_t*)msg_buf;
        this->scope_msg_queues[msg->id].push(msg_buf);
        spdlog::debug(
            "ImscopeConsumer: Received scope message for scope id {} (frame "
            "{}, slot {})",
            msg->id, msg->meta.frame, msg->meta.slot);
      }
    } catch (const std::exception& e) {
      // Log the exception
      fprintf(stderr, "Exception in consumer thread: %s\n", e.what());
      std::abort();
    } catch (...) {
      fprintf(stderr, "Unknown exception in consumer thread\n");
      std::abort();
    }
  }).detach();
}

ImscopeConsumer::ImscopeConsumer(const char* data_address,
                                 const char* control_address, int num_scopes,
                                 imscope_scope_config_t* scopes,
                                 const char* name)
    : data_address(data_address),
      control_address(control_address),
      configured_scopes(scopes, scopes + num_scopes),
      name(name) {

  scope_msg_queues = std::vector<SafePtrQueue>(configured_scopes.size());
  for (int i = 0; i < configured_scopes.size(); i++) {
    scope_msg_queues[i] = SafePtrQueue(1000);
  }
  this->control_socket = create_nn_push_socket(control_address);
  start_consumer_thread();
}

ImscopeConsumer* ImscopeConsumer::connect(const char* announce_address) {
  int req_sock = nn_socket(AF_SP, NN_REQ);
  if (req_sock < 0) {
    return nullptr;
  }
  if (nn_connect(req_sock, announce_address) < 0) {
    return nullptr;
  }

  int timeout = 2000;  // milliseconds
  nn_setsockopt(req_sock, NN_SOL_SOCKET, NN_RCVTIMEO, &timeout,
                sizeof(timeout));

  char* msg_buf = NULL;
  size_t size = sizeof(announce_request_t);
  msg_buf = (char*)malloc(size);
  if (!msg_buf) {
    FatalError("Failed to allocate memory for announce message");
  }

  announce_request_t* announce_msg = (announce_request_t*)msg_buf;
  announce_msg->magic = ANNOUNCE_MSG_ID;
  nn_send(req_sock, msg_buf, size, 0);
  int bytes = nn_recv(req_sock, &msg_buf, NN_MSG, 0);
  if (bytes < 0) {
    return nullptr;
  }

  announce_response_t* response = (announce_response_t*)msg_buf;
  print_announce_response(response);
  auto consumer = new ImscopeConsumer(
      response->data_address, response->control_address, response->num_scopes,
      response->scopes, response->name);
  nn_freemsg(msg_buf);
  nn_close(req_sock);
  return consumer;
}

NnMsgPtr ImscopeConsumer::try_collect_scope_msg(int scope_id, int &version) {
  return scope_msg_queues[scope_id].front(version);
}

void ImscopeConsumer::request_scope_data(int scope_id, int credits) {
  spdlog::debug(
      "ImscopeConsumer: Requesting {} credits for scope {} at address {}",
      credits, scope_id, control_address);
  control_msg_t msg = {.id = scope_id, .credits = credits};
  nn_send(control_socket, &msg, sizeof(msg), 0);
}

void ImscopeConsumer::free(scope_msg_t* msg) {
  nn_freemsg(msg);
}