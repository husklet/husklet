// The checkpoint implementation intentionally remains one translation unit: its
// private state and helpers are shared across the ordered capability fragments.
// Keeping this assembly file small makes each capability independently reviewable
// without widening the native ABI.
#include "checkpoint/capture.c"
#include "checkpoint/ipc_lock_state.c"
#include "checkpoint/image.c"
#include "checkpoint/memory_restore.c"
#include "checkpoint/process_restore.c"
#include "checkpoint/resource_restore.c"
#include "checkpoint/socket_restore.c"
