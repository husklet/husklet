#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>

static int call_errno(int id, int command, void *ignored) {
    errno = 0;
    return shmctl(id, command, ignored) == 0 ? 0 : errno;
}

int main(void) {
    int id = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    if (id < 0) return 1;

    struct shmid_ds before;
    struct shmid_ds after;
    memset(&before, 0, sizeof before);
    memset(&after, 0, sizeof after);
    int stat_before = shmctl(id, IPC_STAT, &before) == 0;
    int lock = call_errno(id, SHM_LOCK, (void *)-1);
    int unlock = call_errno(id, SHM_UNLOCK, (void *)1);
    int stat_after = shmctl(id, IPC_STAT, &after) == 0;
    int unchanged = stat_before && stat_after && before.shm_perm.uid == after.shm_perm.uid &&
                    before.shm_perm.gid == after.shm_perm.gid && before.shm_perm.mode == after.shm_perm.mode &&
                    before.shm_ctime == after.shm_ctime && before.shm_atime == after.shm_atime &&
                    before.shm_dtime == after.shm_dtime && before.shm_nattch == after.shm_nattch;

    int stale = id;
    int removed = shmctl(id, IPC_RMID, NULL) == 0;
    int removed_lock = call_errno(stale, SHM_LOCK, NULL);
    int replacement = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    int stale_unlock = call_errno(stale, SHM_UNLOCK, NULL);
    int invalid = call_errno(-1, SHM_LOCK, NULL);

    printf("sysv_shm_lock lock=%d unlock=%d ignored=%d unchanged=%d removed=%d removed_einval=%d stale_einval=%d "
           "invalid_einval=%d\n",
           lock == 0, unlock == 0, lock != EFAULT && unlock != EFAULT, unchanged, removed, removed_lock == EINVAL,
           stale_unlock == EINVAL, invalid == EINVAL);
    if (replacement >= 0) shmctl(replacement, IPC_RMID, NULL);
    return 0;
}
