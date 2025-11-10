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

int imscope_init_producer(const char* control_address, const char* data_address,
                          const char* announce_address, const char* name,
                          imscope_scope_desc_t* scopes, size_t num_scopes);
int imscope_send_data(uint32_t* data, int id, size_t num_samples, int frame,
                      int slot, uint64_t timestamp);
#ifdef __cplusplus
}
#endif

#endif
