#ifndef HL_HOST_FORK_WIRE_H
#define HL_HOST_FORK_WIRE_H

#include <stddef.h>
#include <stdint.h>

/* A connected named forkserver endpoint. Its representation is host-private. */
typedef struct hl_host_fork_client {
    uintptr_t private_value;
} hl_host_fork_client;

#define HL_HOST_FORK_CLIENT_INIT ((hl_host_fork_client){0})

int hl_host_fork_client_open(hl_host_fork_client *client, const char *path);
/* Sends one launch request and attaches this process's standard streams. */
int hl_host_fork_client_send_launch(hl_host_fork_client *client, const void *buffer, size_t size);
int hl_host_fork_client_receive(hl_host_fork_client *client, void *buffer, size_t size);
void hl_host_fork_client_close(hl_host_fork_client *client);

/* POSIX forkserver transport. Native descriptors never cross into the portable codec. */
int hl_fork_wire_send_descriptors(int socket, const void *buffer, size_t size, const int *descriptors,
                                  int descriptor_count);
int hl_fork_wire_receive_descriptors(int socket, void *buffer, size_t size, int *descriptors, int *descriptor_count);
int hl_fork_wire_send(int socket, const void *buffer, size_t size);
int hl_fork_wire_receive(int socket, void *buffer, size_t size);

#endif
