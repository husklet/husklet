// #421 stress: proves the hl-internal SysV registry is NOT the old host 32-slot table -- it allocates and
// uses WELL more than 32 shared-memory segments concurrently, then a cross-fork shmat/shmdt shared write
// (a child writes a segment the parent reads back through the SAME physical pages), a cross-process
// BLOCKING semop round-trip (the parent P()s a 0-valued semaphore until the child V()s it), and a
// cross-process BLOCKING msgsnd/msgrcv round-trip (the parent blocks in msgrcv on an empty queue until the
// child sends). Verdict-only (booleans), so it is byte-identical on hl and the native Linux oracle -- and
// it would have FAILED on the pre-#421 host-backed path (host shmmni=32, and shm/eventfd not shared cross
// process). Portable POSIX; oracle-diffed vs native aarch64 / qemu x86_64.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/sem.h>
#include <sys/msg.h>

#define NSEG 64 // > 32: the old host table (kern.sysv.shmmni=32) could never hold this many

#if !defined(__APPLE__)
union semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
};
#endif

int main(void) {
    // (1) --- more than 32 concurrent shm segments, each attached + written + read back ---
    int ids[NSEG];
    int *maps[NSEG];
    int created = 0, mapped = 0, dataok = 1;
    for (int i = 0; i < NSEG; i++) {
        ids[i] = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
        if (ids[i] < 0) break;
        created++;
        maps[i] = (int *)shmat(ids[i], 0, 0);
        if (maps[i] == (int *)-1) break;
        mapped++;
        maps[i][0] = i * 7 + 1;
    }
    for (int i = 0; i < mapped; i++)
        if (maps[i][0] != i * 7 + 1) dataok = 0;
    printf("many_segs over32=%d allmapped=%d dataok=%d\n", created > 32, mapped == created && mapped > 32, dataok);

    // free all but segment 0 (kept attached for the cross-fork test)
    for (int i = 1; i < mapped; i++) shmdt(maps[i]);
    for (int i = 1; i < created; i++) shmctl(ids[i], IPC_RMID, 0);

    int seg = ids[0];
    int *shared = maps[0];
    shared[0] = 0;
    shared[1] = 0;

    // (2) a 0-valued semaphore for the child->parent handshake; (3) a queue for the msg round-trip
    int sid = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    union semun su;
    su.val = 0;
    semctl(sid, 0, SETVAL, su);
    int qid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);

    pid_t pid = fork();
    if (pid == 0) {
        // child: attach the SAME segment BY ID (unrelated mapping), write, detach, then release the parent
        int *cs = (int *)shmat(seg, 0, 0);
        if (cs != (int *)-1) {
            cs[0] = 0xABCD;
            cs[1] = 0x1234;
            shmdt(cs);
        }
        struct sembuf up = {0, +1, 0};
        semop(sid, &up, 1); // V: unblocks the parent's P below
        struct {
            long mtype;
            char b[16];
        } m;
        m.mtype = 7;
        strcpy(m.b, "from-child");
        msgsnd(qid, &m, 11, 0);
        _exit(0);
    }

    // parent: BLOCK on the semaphore (0-valued) until the child V()s it -> cross-process blocking semop
    struct sembuf down = {0, -1, 0};
    int sop = semop(sid, &down, 1);
    // the child wrote the segment before posting, so its write is now visible in the parent's mapping
    int shm_shared = (shared[0] == 0xABCD && shared[1] == 0x1234);
    // parent: BLOCK in msgrcv on the (initially empty) queue until the child sends -> cross-process msg
    struct {
        long mtype;
        char b[16];
    } r;
    memset(&r, 0, sizeof r);
    long rn = msgrcv(qid, &r, 16, 7, 0);
    int msg_ok = (rn == 11 && strcmp(r.b, "from-child") == 0);

    int st;
    waitpid(pid, &st, 0);
    printf("xfork shm_shared=%d sem_blockwait=%d msg_roundtrip=%d\n", shm_shared, sop == 0, msg_ok);

    shmdt(shared);
    shmctl(seg, IPC_RMID, 0);
    semctl(sid, 0, IPC_RMID, su);
    msgctl(qid, IPC_RMID, 0);
    return 0;
}
