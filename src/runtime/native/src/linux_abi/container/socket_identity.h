#ifndef HL_LINUX_ABI_CONTAINER_SOCKET_IDENTITY_H
#define HL_LINUX_ABI_CONTAINER_SOCKET_IDENTITY_H

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define HL_SOCKET_IDENTITY_PREFIX "/tmp/.hl-ci-"
#define HL_SOCKET_IDENTITY_PATH_SIZE 46

static inline int hl_socket_identity_format(char *path, size_t capacity, uint64_t client, uint64_t server) {
    if (!path || capacity < HL_SOCKET_IDENTITY_PATH_SIZE || !client || !server || client == server) return -1;
    int length = snprintf(path, capacity, HL_SOCKET_IDENTITY_PREFIX "%016llx-%016llx", (unsigned long long)client,
                          (unsigned long long)server);
    return length == HL_SOCKET_IDENTITY_PATH_SIZE - 1 ? 0 : -1;
}

static inline int hl_socket_identity_parse(const char *path, uint64_t *client, uint64_t *server) {
    if (!path || !client || !server ||
        strncmp(path, HL_SOCKET_IDENTITY_PREFIX, sizeof(HL_SOCKET_IDENTITY_PREFIX) - 1) != 0 ||
        strlen(path) != HL_SOCKET_IDENTITY_PATH_SIZE - 1)
        return -1;
    unsigned long long parsed_client = 0, parsed_server = 0;
    char tail = 0;
    if (sscanf(path + sizeof(HL_SOCKET_IDENTITY_PREFIX) - 1, "%16llx-%16llx%c", &parsed_client, &parsed_server,
               &tail) != 2 ||
        !parsed_client || !parsed_server || parsed_client == parsed_server)
        return -1;
    *client = (uint64_t)parsed_client;
    *server = (uint64_t)parsed_server;
    return 0;
}

#endif
