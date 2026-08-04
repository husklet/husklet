// SysV IPC control-command fidelity (shmctl/semctl/msgctl) — the full IPC_STAT/IPC_SET/*_INFO/*_STAT +
// errno surface behind task #418, diffed vs the native oracle (native aarch64 / qemu x86_64). Verdict-only:
// every line is a boolean or an errno NAME derived from a *self-relative* comparison (fields vs our own
// getuid(), sizes we chose), never a raw id/pid/uid — so hl (guest runs as container-root, uid 0) and the
// native oracle (an unprivileged user) must print byte-identical output. Complements ext_ipc/ipc_sysv_edge.c
// (get/EEXIST/ENOENT) and ipc_sysv_{shm,sem,msg}.c (data round-trips).
//
// The permission paths (EACCES/EPERM) are the subtle case: hl is root, the oracle is not. We normalise by
// dropping to a non-root uid on the privileged (hl) side, then asserting each op returns the result correct
// for the CURRENT privilege — EACCES when a non-root caller has no mode bits (true for a non-root *owner* of
// a mode-0 object on the oracle AND a non-root *non-owner* on hl), and EPERM for IPC_SET/RMID only when we
// are a non-owner (hl, after the drop), owner-allowed otherwise (the oracle). Booleans keep both identical.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/mman.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <unistd.h>

union semun {
    int val;
    struct semid_ds *buf;
    unsigned short *array;
    struct seminfo *__buf;
};

static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case EINVAL: return "EINVAL";
    case EFAULT: return "EFAULT";
    case EACCES: return "EACCES";
    case EPERM: return "EPERM";
    default: return "OTHER";
    }
}

int main(void) {
    void *bad = (void *)-1; // a guaranteed-unmapped buffer for the EFAULT paths
    uid_t me = getuid();

    // ============================ shared memory ============================
    int sid = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    struct shmid_ds sds;
    memset(&sds, 0, sizeof sds);
    int st = shmctl(sid, IPC_STAT, &sds) == 0;
    printf("shm_stat ok=%d segok=%d mode600=%d owner=%d\n", st, sds.shm_segsz >= 4096,
           (sds.shm_perm.mode & 0777) == 0600, sds.shm_perm.uid == me);
    sds.shm_perm.mode = (sds.shm_perm.mode & ~0777) | 0640; // IPC_SET: change the perm bits, read back
    int ss = shmctl(sid, IPC_SET, &sds) == 0;
    memset(&sds, 0, sizeof sds);
    shmctl(sid, IPC_STAT, &sds);
    printf("shm_set ok=%d mode640=%d\n", ss, (sds.shm_perm.mode & 0777) == 0640);

    // ============================ semaphores ============================
    int semid = semget(IPC_PRIVATE, 3, IPC_CREAT | 0600);
    struct semid_ds semds;
    union semun su;
    memset(&semds, 0, sizeof semds);
    su.buf = &semds;
    int est = semctl(semid, 0, IPC_STAT, su) == 0;
    printf("sem_stat ok=%d nsems=%d mode600=%d owner=%d\n", est, semds.sem_nsems == 3,
           (semds.sem_perm.mode & 0777) == 0600, semds.sem_perm.uid == me);
    semds.sem_perm.mode = (semds.sem_perm.mode & ~0777) | 0644;
    su.buf = &semds;
    int eset = semctl(semid, 0, IPC_SET, su) == 0;
    memset(&semds, 0, sizeof semds);
    su.buf = &semds;
    semctl(semid, 0, IPC_STAT, su);
    printf("sem_set ok=%d mode644=%d\n", eset, (semds.sem_perm.mode & 0777) == 0644);

    // ============================ message queue ============================
    int qid = msgget(IPC_PRIVATE, IPC_CREAT | 0600);
    struct msqid_ds mds;
    memset(&mds, 0, sizeof mds);
    int mst = msgctl(qid, IPC_STAT, &mds) == 0;
    printf("msg_stat ok=%d qbytes_pos=%d qnum0=%d mode600=%d owner=%d\n", mst, mds.msg_qbytes > 0,
           mds.msg_qnum == 0, (mds.msg_perm.mode & 0777) == 0600, mds.msg_perm.uid == me);
    mds.msg_perm.mode = (mds.msg_perm.mode & ~0777) | 0620;
    mds.msg_qbytes = 1024; // lowering the queue limit is allowed for a non-privileged owner
    int mset = msgctl(qid, IPC_SET, &mds) == 0;
    memset(&mds, 0, sizeof mds);
    msgctl(qid, IPC_STAT, &mds);
    printf("msg_set ok=%d qbytes1024=%d mode620=%d\n", mset, mds.msg_qbytes == 1024,
           (mds.msg_perm.mode & 0777) == 0620);

    // ============================ IPC_INFO / *_INFO (success >= 0) ============================
    struct shminfo shmi;
    struct shm_info shmnfo;
    struct seminfo semi;
    struct msginfo msgi;
    int i_shm = shmctl(0, IPC_INFO, (struct shmid_ds *)&shmi) >= 0;
    int i_shminfo = shmctl(0, SHM_INFO, (struct shmid_ds *)&shmnfo) >= 0;
    su.__buf = &semi;
    int i_sem = semctl(0, 0, IPC_INFO, su) >= 0;
    su.__buf = &semi;
    int i_seminfo = semctl(0, 0, SEM_INFO, su) >= 0;
    int i_msg = msgctl(0, IPC_INFO, (struct msqid_ds *)&msgi) >= 0;
    int i_msginfo = msgctl(0, MSG_INFO, (struct msqid_ds *)&msgi) >= 0;
    printf("info shmi=%d shminfo=%d semi=%d seminfo=%d msgi=%d msginfo=%d\n", i_shm, i_shminfo, i_sem,
           i_seminfo, i_msg, i_msginfo);

    // ============================ EINVAL on an unknown command ============================
    int c_shm = shmctl(sid, 0x2345, &sds) < 0 ? errno : 0;
    int c_sem = semctl(semid, 0, 0x2345) < 0 ? errno : 0;
    int c_msg = msgctl(qid, 0x2345, &mds) < 0 ? errno : 0;
    printf("einval shm=%s sem=%s msg=%s\n", en(c_shm), en(c_sem), en(c_msg));

    // ============================ EINVAL on an out-of-range *_STAT index ============================
    // Use a NEGATIVE index: *_STAT reads its arg as a kernel array index, and a positive OOR value like
    // 0x40000000 maps via `idx % IPCMNI` back onto a REAL low index (0x40000000 % 32768 == 0) — so on the
    // native oracle it can spuriously find whatever object a *concurrent* test left at host index 0 in the
    // SHARED host SysV table (the exact contention #421 removes on hl's side) and return "ok". A negative
    // index is out of range on both hl and any real kernel's IDR, so this stays deterministic under
    // concurrency while testing the same thing (an out-of-range *_STAT index -> EINVAL).
    int x_shm = shmctl(-1, SHM_STAT, &sds) < 0 ? errno : 0;
    su.buf = &semds;
    int x_sem = semctl(-1, 0, SEM_STAT, su) < 0 ? errno : 0;
    int x_msg = msgctl(-1, MSG_STAT, &mds) < 0 ? errno : 0;
    printf("badidx shm=%s sem=%s msg=%s\n", en(x_shm), en(x_sem), en(x_msg));

    // ============================ EFAULT on a bad IPC_STAT buffer ============================
    int f_shm = shmctl(sid, IPC_STAT, (struct shmid_ds *)bad) < 0 ? errno : 0;
    su.buf = (struct semid_ds *)bad;
    int f_sem = semctl(semid, 0, IPC_STAT, su) < 0 ? errno : 0;
    int f_msg = msgctl(qid, IPC_STAT, (struct msqid_ds *)bad) < 0 ? errno : 0;
    printf("efault shm=%s sem=%s msg=%s\n", en(f_shm), en(f_sem), en(f_msg));

    // ============================ EACCES / EPERM permission paths ============================
    int acc_id = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0000); // mode 0 -> denied to any non-root caller
    int rmid_id = shmget(IPC_PRIVATE, 4096, IPC_CREAT | 0600);
    int psem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
    int pmsg = msgget(IPC_PRIVATE, IPC_CREAT | 0600);

    int dropped = 0;
    if (geteuid() == 0) { // hl runs as root: drop so the container mode/owner check actually applies
        seteuid(65534);
        dropped = 1;
    }
    int want_owner = dropped ? EPERM : 0; // hl(dropped)=non-owner -> EPERM; oracle=owner -> allowed

    struct shmid_ds ab;
    int acc = shmctl(acc_id, IPC_STAT, &ab) < 0 ? errno : 0; // EACCES on both (mode-0, non-root)
    struct shmid_ds sb;
    memset(&sb, 0, sizeof sb);
    sb.shm_perm.uid = geteuid();
    sb.shm_perm.mode = 0644;
    int p_set = shmctl(rmid_id, IPC_SET, &sb) < 0 ? errno : 0;
    int p_rmid = shmctl(rmid_id, IPC_RMID, NULL) < 0 ? errno : 0;
    struct semid_ds spb;
    memset(&spb, 0, sizeof spb);
    spb.sem_perm.uid = geteuid();
    spb.sem_perm.mode = 0644;
    union semun psu;
    psu.buf = &spb;
    int p_semset = semctl(psem, 0, IPC_SET, psu) < 0 ? errno : 0;
    struct msqid_ds mpb;
    memset(&mpb, 0, sizeof mpb);
    mpb.msg_perm.uid = geteuid();
    mpb.msg_perm.mode = 0644;
    mpb.msg_qbytes = 512;
    int p_msgset = msgctl(pmsg, IPC_SET, &mpb) < 0 ? errno : 0;

    printf("perm acc=%d shmset=%d shmrmid=%d semset=%d msgset=%d\n", acc == EACCES, p_set == want_owner,
           p_rmid == want_owner, p_semset == want_owner, p_msgset == want_owner);

    if (dropped) seteuid(0); // regain root to clean everything up
    shmctl(acc_id, IPC_RMID, NULL);
    shmctl(rmid_id, IPC_RMID, NULL); // no-op on the oracle (already removed above); cleans up on hl
    semctl(psem, 0, IPC_RMID);
    msgctl(pmsg, IPC_RMID, NULL);
    shmctl(sid, IPC_RMID, NULL);
    semctl(semid, 0, IPC_RMID);
    msgctl(qid, IPC_RMID, NULL);
    return 0;
}
