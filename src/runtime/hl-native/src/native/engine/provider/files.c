#include "files.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

static hl_host_result unsupported(void) {
    return (hl_host_result){.status = HL_STATUS_NOT_SUPPORTED};
}

int hl_provider_files_install(hl_host_services *services, hl_provider_client *client) {
    (void)services;
    (void)client;
    return -ENOTSUP;
}

hl_host_result hl_provider_files_open_service(uint64_t service, uint32_t access) {
    (void)service;
    (void)access;
    return unsupported();
}

void hl_provider_files_revoke(void) {
}

int hl_provider_files_is_handle(hl_host_handle handle) {
    (void)handle;
    return 0;
}

hl_host_result hl_provider_files_ioctl(hl_host_handle handle, uint64_t command, unsigned char *argument, uint32_t size,
                                       hl_provider_ioctl_result *output) {
    (void)handle;
    (void)command;
    (void)argument;
    (void)size;
    if (output != NULL) memset(output, 0, sizeof(*output));
    return unsupported();
}

void hl_provider_files_ioctl_result_destroy(hl_provider_ioctl_result *result) {
    if (result == NULL) return;
    for (uint32_t index = 0; index < result->write_count && index < HL_PROVIDER_IOCTL_WRITE_MAX; ++index)
        free(result->writes[index].bytes);
    memset(result, 0, sizeof(*result));
}

uint32_t hl_provider_files_readiness(hl_host_handle handle, uint32_t interests) {
    (void)handle;
    (void)interests;
    return 0;
}

uint32_t hl_provider_files_cached_readiness(hl_host_handle handle, uint32_t interests) {
    (void)handle;
    (void)interests;
    return 0;
}

int hl_provider_files_subscribe(hl_host_handle handle, uint32_t interests, void (*notify)(void *, uint64_t),
                                void *opaque, uint64_t token) {
    (void)handle;
    (void)interests;
    (void)notify;
    (void)opaque;
    (void)token;
    return -ENOTSUP;
}

void hl_provider_files_unsubscribe(hl_host_handle handle, void *opaque, uint64_t token) {
    (void)handle;
    (void)opaque;
    (void)token;
}
