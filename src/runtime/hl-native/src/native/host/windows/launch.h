#ifndef HL_HOST_WINDOWS_LAUNCH_H
#define HL_HOST_WINDOWS_LAUNCH_H

/*
 * Carrying a launch across a cold child on a Windows host.
 *
 * The process group's spawn is a CreateProcess re-execution of this image, not
 * an address-space clone, so the entry context it takes is a bare pointer into
 * memory the child does not have. A caller whose context is a single scalar (or
 * NULL) crosses as it stands and needs nothing here. A caller whose context is a
 * graph of parent pointers -- a config, an argv, an option store, an executable
 * image -- has to hand the child BYTES, and this is the channel that does it.
 *
 * Two things travel, and they travel by different means because they are
 * different kinds of thing:
 *
 *   - the payload is COPIED. It is a snapshot the child owns; the parent may
 *     free its originals the moment the spawn returns. It rides in the same
 *     inherited section the spawn's launch record already uses, so it costs no
 *     new object and no new handle.
 *   - the shared mapping is HANDED OVER as a handle. CreateProcess inherits
 *     handles, not views, so a parent address for shared memory is meaningless
 *     in the child; what crosses is the section, and the child maps a view of
 *     its own. That is what lets a child write a result the parent reads after
 *     waiting for it.
 *
 * The encoding of the payload is entirely the caller's business. Nothing in the
 * host backend reads a byte of it: the producer and the consumer are the same
 * image, launched from the same file, so the caller is free to use an encoding
 * that only that image can interpret.
 *
 * Publishing is per-thread and is consumed by the very next spawn on that
 * thread. That is the narrowest lifetime that works -- two threads spawning at
 * once cannot take each other's payload, and a spawn that fails leaves nothing
 * behind for the next one to pick up.
 */

#include "hl/host_services.h"

#include <stddef.h>

HL_EXTERN_C_BEGIN

/*
 * Parent side. `bytes` is BORROWED: it is read during the spawn that follows and
 * never after, so the publisher owns it across that one call and may release it
 * immediately afterwards. `shared` is a mapping handle from this host's memory
 * group, created HL_HOST_MEMORY_SHARED; HL_HOST_HANDLE_INVALID hands over none.
 * Passing size 0 and HL_HOST_HANDLE_INVALID clears a publication that a caller
 * decided not to spawn on.
 */
hl_status hl_host_windows_launch_publish(const void *bytes, size_t size, hl_host_handle shared);

/*
 * Child side. Both return NULL in a process that was not spawned this way, which
 * is how an entry point tells a cold launch from every other way it can be
 * reached. The payload stays valid for the life of the process.
 */
const void *hl_host_windows_launch_payload(size_t *out_size);
void *hl_host_windows_launch_shared(size_t *out_size);

HL_EXTERN_C_END

#endif
