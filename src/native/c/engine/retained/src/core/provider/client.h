#ifndef HL_CORE_PROVIDER_CLIENT_H
#define HL_CORE_PROVIDER_CLIENT_H

#include <pthread.h>
#include <stddef.h>
#include <stdint.h>

typedef struct hl_provider_client {
    int descriptor;
    uint32_t maximum_payload;
    struct hl_provider_demux *demux;
    int initialized;
} hl_provider_client;

typedef struct hl_provider_reply {
    unsigned char *bytes;
    uint32_t size;
    int linux_errno;
} hl_provider_reply;

typedef void (*hl_provider_client_wake_fn)(void *context, uint64_t subscription);

int hl_provider_client_init(hl_provider_client *client, int descriptor, uint32_t maximum_payload);
/* Supports concurrent copied requests; one internal reader correlates replies. */
int hl_provider_client_request(hl_provider_client *client, const void *bytes, uint32_t size, uint32_t timeout_ms,
                               hl_provider_reply *reply);
int hl_provider_client_subscribe(hl_provider_client *client, uint64_t subscription, const void *bytes, uint32_t size,
                                 hl_provider_client_wake_fn wake, void *context);
int hl_provider_client_readiness(hl_provider_client *client, uint64_t subscription, hl_provider_reply *event,
                                 uint64_t *lost);
int hl_provider_client_unsubscribe(hl_provider_client *client, uint64_t subscription);
void hl_provider_reply_destroy(hl_provider_reply *reply);
void hl_provider_client_destroy(hl_provider_client *client);

#endif
