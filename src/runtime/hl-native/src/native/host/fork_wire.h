#ifndef HL_HOST_FORK_WIRE_H
#define HL_HOST_FORK_WIRE_H

#include <stddef.h>
/* Local descriptor transport shared by checkpoint broker endpoints. */
int hl_fork_wire_send_descriptors(int socket, const void *buffer, size_t size, const int *descriptors,
                                  int descriptor_count);
int hl_fork_wire_receive_descriptors(int socket, void *buffer, size_t size, int *descriptors, int *descriptor_count);
int hl_fork_wire_send(int socket, const void *buffer, size_t size);
int hl_fork_wire_receive(int socket, void *buffer, size_t size);

#endif
