#ifndef HL_CORE_PROVIDER_DEMUX_H
#define HL_CORE_PROVIDER_DEMUX_H

#include "client.h"

#include <stdint.h>

typedef struct hl_provider_demux hl_provider_demux;

typedef struct hl_provider_ticket {
    uint64_t request;
} hl_provider_ticket;

typedef struct hl_provider_event {
    unsigned char *bytes;
    uint32_t size;
} hl_provider_event;

typedef void (*hl_provider_wake_fn)(void *context, uint64_t subscription);

/* The descriptor remains owned by the caller.  Exactly one reader thread owns
 * reads from it for the lifetime of the demultiplexer. */
int hl_provider_demux_create(hl_provider_demux **out, int descriptor, uint32_t maximum_payload,
                             uint32_t maximum_waiters, uint32_t maximum_subscriptions, uint32_t event_capacity);
int hl_provider_demux_begin(hl_provider_demux *demux, const void *bytes, uint32_t size, hl_provider_ticket *ticket);
int hl_provider_demux_wait(hl_provider_demux *demux, hl_provider_ticket ticket, uint32_t timeout_ms,
                           hl_provider_reply *reply);
int hl_provider_demux_cancel(hl_provider_demux *demux, hl_provider_ticket ticket);

int hl_provider_demux_subscribe(hl_provider_demux *demux, uint64_t subscription, hl_provider_wake_fn wake,
                                void *context);
int hl_provider_demux_subscribe_remote(hl_provider_demux *demux, uint64_t subscription, const void *bytes,
                                       uint32_t size, hl_provider_wake_fn wake, void *context);
int hl_provider_demux_next(hl_provider_demux *demux, uint64_t subscription, hl_provider_event *event, uint64_t *lost);
int hl_provider_demux_unsubscribe(hl_provider_demux *demux, uint64_t subscription);
int hl_provider_demux_unsubscribe_remote(hl_provider_demux *demux, uint64_t subscription);
void hl_provider_event_destroy(hl_provider_event *event);

/* Stops I/O, joins the sole reader, broadcasts peer closure, then releases all
 * queued payloads and synchronization objects. */
void hl_provider_demux_destroy(hl_provider_demux *demux);

#endif
