/* hl/core -- the engine-process side of the checkpoint stream transport (hl/checkpoint_stream.h).
 *
 * Lives in core rather than in linux_abi because two separately linked translation units need it: the
 * activation child (src/core/activation.c), which receives the broker descriptor over SCM_RIGHTS and
 * publishes it here, and the checkpoint writer (src/linux_abi/checkpoint.c, compiled into the per-target
 * unity TU), which consumes it. It is deliberately free of engine state: a descriptor and a per-process
 * lazily created channel, nothing else. */

#ifndef HL_CORE_CHECKPOINT_CHANNEL_H
#define HL_CORE_CHECKPOINT_CHANNEL_H

#include <stddef.h>
#include <stdint.h>

#include "hl/activation.h"
#include "hl/checkpoint_stream.h"

/* Activation publishes the inherited broker descriptor exactly once, before the guest starts. It is
 * inherited by every fork() of the engine, which is what lets a peer process reach the embedder's store. */
void hl_ckpt_channel_publish(int broker);
int hl_ckpt_channel_broker(void);

/* Adopt an already-inherited broker/trigger pair given as decimal descriptor numbers. This is the standalone
 * engine's equivalent of what activation does over SCM_RIGHTS: the descriptors are registered as
 * engine-private so the guest descriptor scan never sees them. Returns 0, or -1 on a malformed argument. */
int hl_ckpt_channel_adopt(const char *broker, const char *trigger);

/* This process's private request/response channel, created on first use and re-created after a fork so a
 * child never shares a channel (and therefore never interleaves a request) with its parent.
 * Returns -1 when no broker was published or the connect failed. */
int hl_ckpt_channel_acquire(void);
int hl_ckpt_channel_authenticate_peer(int descriptor, uint64_t claimed_pid, uint64_t *authenticated_pid);
#if defined(HL_NATIVE_TEST_HOOKS)
void hl_ckpt_channel_test_claimed_pid(uint64_t claimed_pid);
void hl_ckpt_channel_forget_for_test(void);
int hl_ckpt_channel_current_for_test(void);
#endif

/* One round trip. `name` (or NULL) is sent NUL-terminated; `payload` is `request->length` bytes. Reply
 * payload is copied into `out` (up to `capacity`); a longer reply is a protocol error. Returns 0 when a
 * well-formed reply arrived -- the operation's own status is in `reply->status` -- and -1 when the transport
 * or the framing failed. */
/* Names the step at which this process's LAST channel round trip failed, or NULL if none has.
 *
 * The transport reports one -1 for four unrelated events -- no broker, a channel that could not be minted,
 * a write that found the far end gone, and a reply that never arrived -- and a caller that prints only the
 * -1 has told the reader nothing. Three lanes have read "answered status -1" as a broker refusal, which is
 * the one thing it is NOT: a refusal carries a status and a reply. Recording the step is diagnostic only
 * and changes no control flow. */
const char *hl_ckpt_channel_failure(void);

/* Whether `descriptor` is one this process's checkpoint transport owns: the broker, the trigger, or this
 * process's channel.
 *
 * The engine already refuses to close its own descriptors on the guest's behalf, and it decides which
 * they are from the engine-private ledger (hl_host_process_fd_private_current). That ledger is keyed by
 * (pid, start time), so a fork CHILD owns no rows until hl_host_process_fd_private_fork_complete has
 * replayed the parent's -- and a child that reaches a guest close_range() before, or without, that replay
 * reads its inherited transport descriptors as ordinary guest fds and closes them. glibc sanitizes the
 * descriptor table with close_range(3, ~0U) in every posix_spawn child, so an ordinary shell running
 * `sleep .05` produces one such child every time; the loss is invisible until a capture catches one of
 * them alive, whereupon its REGISTER_READY dies at sendmsg with EBADF, that member refuses its own dump,
 * and the whole close is refused for a healthy tree.
 *
 * These three descriptors are held in this file's own statics, so ownership can be answered here without
 * consulting anything a fork may not have rebuilt yet. Answering it from the owner rather than from a
 * derived index is what makes the answer independent of when the ledger is repopulated. */
int hl_ckpt_channel_owns_descriptor(int descriptor);

int hl_ckpt_channel_call(hl_ckpt_request *request, const char *name, const void *payload, hl_ckpt_reply *reply,
                         void *out, size_t capacity);

/* One round trip whose reply may carry a descriptor over SCM_RIGHTS. The request is framed exactly as
 * hl_ckpt_channel_call frames it; only the reply is read with recvmsg, because a plain read() would take
 * the header and DISCARD the rights attached to it. The reply carries no payload.
 *
 * `*out_descriptor` is set to the received descriptor, or left at -1 when the server answered without one
 * -- which is an ordinary answer, not a failure, and is how the server declines a request it has nothing
 * registered for. Returns 0 when a well-formed reply arrived and -1 on a transport or framing failure. */
int hl_ckpt_channel_call_receive_descriptor(hl_ckpt_request *request, const void *payload, hl_ckpt_reply *reply,
                                            int *out_descriptor);

/* The checkpoint TRIGGER is a 4-byte generation counter shared by every engine process and bumped by the
 * embedder to request a capture. ckpt_poll reads it at every safepoint, so it has to be a plain memory load;
 * it cannot be a message. It is an anonymous shared mapping whose descriptor activation hands to the engine
 * exactly like the broker. */
void hl_ckpt_trigger_publish(int descriptor);
int hl_ckpt_trigger_descriptor(void);

/* Embedder side, called from the Rust FFI boundary. `hl_ckpt_broker_pair` creates the datagram socketpair
 * whose child end is handed to activation; `hl_ckpt_broker_accept` waits up to `timeout_ms` for one engine
 * process to announce itself and returns its channel descriptor (HL_ACTIVATION_DESCRIPTOR_NONE on timeout
 * or error).
 *
 * These four carry hl_activation_descriptor rather than int because their values cross the same boundary
 * as activation's, and two of them are handed to activation directly: the broker's child end and the
 * trigger's descriptor are two of the three descriptors hl_activation_start_with_streams attaches. One
 * spelling of "absent" on both sides of that hand-off, and it is not -1. The engine-internal entry points
 * above keep plain int -- they never leave this process, and their -1 is a POSIX descriptor number rather
 * than an API contract. */
int hl_ckpt_broker_pair(hl_activation_descriptor *out_parent, hl_activation_descriptor *out_child);
hl_activation_descriptor hl_ckpt_broker_accept(hl_activation_descriptor broker, int timeout_ms, uint64_t *out_host_pid);

/* Embedder side of the trigger: create the shared counter, read and bump it, release it. */
int hl_ckpt_trigger_create(hl_activation_descriptor *out_descriptor, void **out_mapping);
uint32_t hl_ckpt_trigger_bump(void *mapping);
void hl_ckpt_trigger_destroy(void *mapping, hl_activation_descriptor descriptor);

#endif
