#pragma once

#include <nanomsg/nn.h>
#include <deque>
#include <queue>
#include <thread>
#include "imscope_common.h"
#include "imscope_consumer.h"
#include "imscope_tools.h"

typedef struct {
  scope_msg_t* msg;

  union {
    IQSnapshot* iq_data;
    VectorSnapshot* vector_data;
  } processed;
} data_record_t;

class Cache {
  SafeQueue<data_record_t> cached_msgs;
  ImscopeConsumer* consumer;
  int scope_id;
  size_t max_messages;

  void start_processing_thread() {
    std::thread([this]() {
      while (1) {
        scope_msg_t* msg = consumer->try_collect_scope_msg(scope_id);
        if (msg) {
          switch (consumer->get_msg_type(scope_id)) {
            case SCOPE_TYPE_IQ_DATA: {
              IQSnapshot* iq_data = new IQSnapshot();
              iq_data->read_scope_msg(msg);
              data_record_t record;
              record.msg = msg;
              record.processed.iq_data = iq_data;
              cached_msgs.push(record);
              break;
            }
            case SCOPE_TYPE_REAL: {
              VectorSnapshot* vector_data = new VectorSnapshot();
              vector_data->read_scope_msg(msg);
              data_record_t record;
              record.msg = msg;
              record.processed.vector_data = vector_data;
              cached_msgs.push(record);
              break;
            }
            default:
              // Unknown type, free the message
              nn_freemsg(msg);
              break;
          }

          if (cached_msgs.size() > max_messages) {
            data_record_t old_record;
            cached_msgs.pop(&old_record);
            nn_freemsg(old_record.msg);
            free(old_record.processed.iq_data);
          }
        } else {
          std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
      }
    }).detach();
  }

 public:
  Cache(int scope_id, ImscopeConsumer* consumer)
      : max_messages(100), scope_id(scope_id), consumer(consumer) {
    start_processing_thread();
  }

  size_t size() { return cached_msgs.size(); }
};
