#define _POSIX_C_SOURCE 200809L
#include "client.h"
#include "demux.h"

#include <errno.h>
#include <stdlib.h>
#include <string.h>

int hl_provider_client_init(hl_provider_client *client, int descriptor, uint32_t maximum_payload) {
    if (client == NULL || descriptor < 0 || maximum_payload == 0) return -EINVAL;
    memset(client, 0, sizeof(*client));
    client->descriptor = descriptor;
    client->maximum_payload = maximum_payload;
    if (hl_provider_demux_create(&client->demux, descriptor, maximum_payload, 64, 64, 64) != 0) return -EIO;
    client->initialized = 1;
    return 0;
}

int hl_provider_client_request(hl_provider_client *client, const void *bytes, uint32_t size, uint32_t timeout_ms,
                               hl_provider_reply *reply) {
    hl_provider_ticket ticket;
    int status;
    if (reply != NULL) memset(reply, 0, sizeof(*reply));
    if (client == NULL || !client->initialized || bytes == NULL || reply == NULL || timeout_ms == 0 ||
        size > client->maximum_payload)
        return -EINVAL;
    status = hl_provider_demux_begin(client->demux, bytes, size, &ticket);
    return status == 0 ? hl_provider_demux_wait(client->demux, ticket, timeout_ms, reply) : status;
}

int hl_provider_client_subscribe(hl_provider_client *client, uint64_t subscription, const void *bytes, uint32_t size,
                                 hl_provider_client_wake_fn wake, void *context) {
    if (client == NULL || !client->initialized) return -EINVAL;
    return hl_provider_demux_subscribe_remote(client->demux, subscription, bytes, size, wake, context);
}

int hl_provider_client_readiness(hl_provider_client *client, uint64_t subscription, hl_provider_reply *event,
                                 uint64_t *lost) {
    hl_provider_event value;
    int status;
    if (client == NULL || !client->initialized || event == NULL || lost == NULL) return -EINVAL;
    memset(event, 0, sizeof(*event));
    status = hl_provider_demux_next(client->demux, subscription, &value, lost);
    if (status == 0) {
        event->bytes = value.bytes;
        event->size = value.size;
    }
    return status;
}

int hl_provider_client_unsubscribe(hl_provider_client *client, uint64_t subscription) {
    if (client == NULL || !client->initialized) return -EINVAL;
    return hl_provider_demux_unsubscribe_remote(client->demux, subscription);
}

void hl_provider_reply_destroy(hl_provider_reply *reply) {
    if (reply == NULL) return;
    free(reply->bytes);
    memset(reply, 0, sizeof(*reply));
}

void hl_provider_client_destroy(hl_provider_client *client) {
    if (client == NULL || !client->initialized) return;
    client->initialized = 0;
    hl_provider_demux_destroy(client->demux);
    client->demux = NULL;
}
