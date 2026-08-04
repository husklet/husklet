// System V IPC errno/edge fidelity (LTP msgget/semget/shmget + *ctl families) — diffed vs the native
// oracle. Verdict-only (errno NAMES + booleans, never raw ids/pids), so hl must be byte-identical to
// native Linux (aarch64) / qemu (x86_64). Complements ext_ipc/ipc_sysv_{shm,sem,msg}.c (round-trips).
// Exercises: IPC_EXCL EEXIST on re-create, ENOENT for a missing key without IPC_CREAT, EINVAL on a bad
// id, the shm data round-trip + IPC_STAT size, sem SETVAL/GETVAL, and msg IPC_NOWAIT ENOMSG on empty.
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <unistd.h>

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case EEXIST: return "EEXIST";
    case ENOENT: return "ENOENT";
    case EINVAL: return "EINVAL";
    case ENOMSG: return "ENOMSG";
    case EACCES: return "EACCES";
    case EIDRM: return "EIDRM";
    default: return "OTHER";
    }
}
static const char *rc(long r) { return r >= 0 ? "ok" : en(errno); }

union semun { int val; struct semid_ds *buf; unsigned short *array; };

int main(void) {
    key_t key = (key_t)(0x0DD0000 + (getpid() & 0xffff));

    // ---------------- SysV shared memory ----------------
    int sid = shmget(key, 4096, IPC_CREAT | IPC_EXCL | 0600);
    printf("shm_create=%s\n", rc(sid));
    // re-create the same key with IPC_EXCL -> EEXIST
    printf("shm_excl=%s\n", en(shmget(key, 4096, IPC_CREAT | IPC_EXCL | 0600) < 0 ? errno : 0));
    // open a *missing* key without IPC_CREAT -> ENOENT
    printf("shm_noent=%s\n", en(shmget(key + 1, 4096, 0600) < 0 ? errno : 0));
    // attach, write, detach, re-attach, read back
    char *p = shmat(sid, NULL, 0);
    int at_ok = p != (char *)-1;
    if (at_ok) { strcpy(p, "sysv-shm-payload"); shmdt(p); }
    char *p2 = at_ok ? shmat(sid, NULL, SHM_RDONLY) : (char *)-1;
    int rt = (p2 != (char *)-1) && strcmp(p2, "sysv-shm-payload") == 0;
    printf("shm_roundtrip=%d\n", rt);
    if (p2 != (char *)-1) shmdt(p2);
    // IPC_STAT reports at least the requested size
    struct shmid_ds sds;
    int stat_ok = shmctl(sid, IPC_STAT, &sds) == 0 && sds.shm_segsz >= 4096;
    printf("shm_stat=%d\n", stat_ok);
    printf("shm_rmid=%s\n", rc(shmctl(sid, IPC_RMID, NULL)));

    // ---------------- SysV semaphores ----------------
    int semid = semget(key, 2, IPC_CREAT | IPC_EXCL | 0600);
    printf("sem_create=%s\n", rc(semid));
    printf("sem_excl=%s\n", en(semget(key, 2, IPC_CREAT | IPC_EXCL | 0600) < 0 ? errno : 0));
    printf("sem_noent=%s\n", en(semget(key + 1, 2, 0600) < 0 ? errno : 0));
    union semun su;
    su.val = 7;
    int setval_ok = semctl(semid, 0, SETVAL, su) == 0;
    int getval = semctl(semid, 0, GETVAL);
    printf("sem_setget=%d\n", setval_ok && getval == 7);
    // semop: decrement by 3 then read back
    struct sembuf op = {0, -3, 0};
    int op_ok = semop(semid, &op, 1) == 0;
    printf("sem_op=%d\n", op_ok && semctl(semid, 0, GETVAL) == 4);
    printf("sem_rmid=%s\n", rc(semctl(semid, 0, IPC_RMID)));

    // ---------------- SysV message queue ----------------
    int qid = msgget(key, IPC_CREAT | IPC_EXCL | 0600);
    printf("msg_create=%s\n", rc(qid));
    printf("msg_excl=%s\n", en(msgget(key, IPC_CREAT | IPC_EXCL | 0600) < 0 ? errno : 0));
    printf("msg_noent=%s\n", en(msgget(key + 1, 0600) < 0 ? errno : 0));
    struct { long mtype; char mtext[16]; } m;
    // receive on an empty queue with IPC_NOWAIT -> ENOMSG
    printf("msg_nowait=%s\n", en(msgrcv(qid, &m, sizeof m.mtext, 0, IPC_NOWAIT) < 0 ? errno : 0));
    // send two typed messages, receive type 2 first (selective), then any
    m.mtype = 1; strcpy(m.mtext, "one");   msgsnd(qid, &m, 4, 0);
    m.mtype = 2; strcpy(m.mtext, "two");   msgsnd(qid, &m, 4, 0);
    memset(&m, 0, sizeof m);
    ssize_t r2 = msgrcv(qid, &m, sizeof m.mtext, 2, 0);
    printf("msg_type2=%d\n", r2 == 4 && strcmp(m.mtext, "two") == 0);
    memset(&m, 0, sizeof m);
    ssize_t r1 = msgrcv(qid, &m, sizeof m.mtext, 0, 0);
    printf("msg_any=%d\n", r1 == 4 && strcmp(m.mtext, "one") == 0);
    printf("msg_rmid=%s\n", rc(msgctl(qid, IPC_RMID, NULL)));
    return 0;
}
