#ifndef HL_LINUX_ABI_HOST_SYSV_H
#define HL_LINUX_ABI_HOST_SYSV_H

/*
 * <sys/ipc.h> + <sys/shm.h> + <sys/sem.h> + <sys/msg.h> for this layer.
 *
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h.
 * On Linux and macOS this is the four system headers; on Windows the
 * vocabulary is synthesized below.
 *
 * THIS FILE IS ALMOST ENTIRELY SHAPE, AND THAT IS THE INTERESTING FACT ABOUT
 * IT.  syscall/sysv.c does not call the host's SysV IPC at all -- on ANY host.
 * It used to, and its own opening note records why it stopped: the macOS SysV
 * table is tiny (kern.sysv.shmmni=32) and, worse, GLOBAL rather than
 * per-container, so real software hit ENOSPC where Linux succeeds, every
 * container in the test matrix shared one 32-slot table, and a killed run
 * leaked segments that filled it.  What replaced it is a complete in-engine
 * emulation: a per-container control block in POSIX shared memory holding the
 * id/key tables, one named shm object per segment, semaphore values and message
 * rings living in that shared memory, and a robust cross-process spinlock.  It
 * carries its own L_IPC_RMID / L_SEM_UNDO / L_GETNCNT constants because those
 * are the GUEST's numbers, which it must honour exactly, and it never needs the
 * host's.
 *
 * So this header has no caller in the tree today.  It exists because
 * syscall/dispatch.c includes all four system headers unconditionally, and a
 * host that has none of them still has to present the words.  That is the same
 * job native_compat.h's Windows arm does for kqueue.
 *
 * Two consequences worth stating plainly rather than leaving to be discovered:
 *
 *   - Because nothing calls these, none of the refusals below is on a path the
 *     guest can reach.  Adding a caller would be a design change, not a port
 *     step: the emulation in syscall/sysv.c is the intended implementation on
 *     every host, and routing a guest shmget() to a host shmget() would
 *     reintroduce exactly the global-table sharing the emulation exists to
 *     avoid.  Windows makes that doubly moot -- it has no SysV IPC of any kind
 *     (its shared memory is a named file mapping, its semaphores are kernel
 *     objects with no undo list and no id namespace, and there is no message
 *     queue primitive with SysV's typed-receive semantics at all).
 *
 *   - The struct layouts still matter even with no caller, because they are the
 *     shapes a future reader will compare the guest's ipc64_perm / shmid64_ds
 *     marshalling against.  They are Linux/x86-64's, field for field.
 *
 * ONE SPELLING NOTE that applies to every struct below.  Linux's declarations
 * use `unsigned long`, `time_t` and `__syscall_ulong_t` for their 64-bit
 * members.  This target is LLP64: `long` is 32 bits, so spelling those fields
 * `long` would silently halve each one and shift the offset of everything after
 * it.  They are written with explicit fixed-width types instead, and the layout
 * that results is the Linux one.  uid_t/gid_t are likewise spelled out as
 * `unsigned int` rather than named -- this seam does not own those typedefs and
 * must not race the header that does.
 */

#if !defined(_WIN32)

#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>

#else /* Windows */

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

struct timespec;

/* ---- SHAPE: <sys/ipc.h>. ------------------------------------------------ */

typedef int key_t;

/*
 * The Linux x86-64 struct ipc_perm -- which is the kernel's ipc64_perm, padding
 * included.  The two `unsigned short` pads after `mode` and `__seq` are not
 * decoration: without them the trailing reserved words land four bytes early
 * and the struct is 44 bytes instead of 48.
 */
struct ipc_perm {
    key_t __key;
    unsigned int uid;  /* uid_t */
    unsigned int gid;  /* gid_t */
    unsigned int cuid; /* uid_t */
    unsigned int cgid; /* gid_t */
    unsigned short mode;
    unsigned short __pad1;
    unsigned short __seq;
    unsigned short __pad2;
    uint64_t __reserved1;
    uint64_t __reserved2;
};

#define IPC_PRIVATE ((key_t)0)

/* get/creation flags */
#define IPC_CREAT 01000
#define IPC_EXCL 02000
#define IPC_NOWAIT 04000

/* control commands */
#define IPC_RMID 0
#define IPC_SET 1
#define IPC_STAT 2
#define IPC_INFO 3

/* ---- SHAPE: <sys/shm.h>. ------------------------------------------------ */

struct shmid_ds {
    struct ipc_perm shm_perm;
    uint64_t shm_segsz; /* size_t */
    int64_t shm_atime;  /* time_t */
    int64_t shm_dtime;
    int64_t shm_ctime;
    int32_t shm_cpid; /* pid_t */
    int32_t shm_lpid;
    uint64_t shm_nattch; /* shmatt_t */
    uint64_t __reserved4;
    uint64_t __reserved5;
};

/* shmat flags */
#define SHM_RDONLY 010000
#define SHM_RND 020000
#define SHM_REMAP 040000
#define SHM_EXEC 0100000

/* shmctl commands beyond the IPC_* set */
#define SHM_LOCK 11
#define SHM_UNLOCK 12
#define SHM_STAT 13
#define SHM_INFO 14
#define SHM_STAT_ANY 15

/* mode bits, as <sys/shm.h> spells them */
#define SHM_R 0400
#define SHM_W 0200

/* SHM_RND rounds an attach address DOWN to a multiple of this.  Linux/x86-64
 * defines SHMLBA as the page size, 4096 -- and that is the guest's number, not
 * the host's.  It is deliberately NOT the 65536 that native_compat.h's
 * getpagesize() reports for Windows: that value is the host mapper's allocation
 * granularity, a different quantity used for a different purpose, and rounding
 * a guest attach address to it would move the address the guest asked for. */
#define SHMLBA 4096

/* ---- SHAPE: <sys/sem.h>. ------------------------------------------------ */

struct semid_ds {
    struct ipc_perm sem_perm;
    int64_t sem_otime; /* time_t */
    uint64_t __reserved1;
    int64_t sem_ctime; /* time_t */
    uint64_t __reserved2;
    uint64_t sem_nsems;
    uint64_t __reserved3;
    uint64_t __reserved4;
};

struct sembuf {
    unsigned short sem_num;
    short sem_op;
    short sem_flg;
};

/* union semun is deliberately absent.  POSIX puts the burden of declaring it on
 * the APPLICATION and glibc's <sys/sem.h> honours that by not declaring it, so
 * a caller written against Linux already carries its own; synthesizing one here
 * would hand that caller a second, conflicting definition. */

#define SEM_UNDO 0x1000

/* semctl commands beyond the IPC_* set */
#define GETPID 11
#define GETVAL 12
#define GETALL 13
#define GETNCNT 14
#define GETZCNT 15
#define SETVAL 16
#define SETALL 17
#define SEM_STAT 18
#define SEM_INFO 19
#define SEM_STAT_ANY 20

/* ---- SHAPE: <sys/msg.h>. ------------------------------------------------ */

struct msqid_ds {
    struct ipc_perm msg_perm;
    int64_t msg_stime; /* time_t */
    int64_t msg_rtime;
    int64_t msg_ctime;
    uint64_t __msg_cbytes;
    uint64_t msg_qnum;   /* msgqnum_t */
    uint64_t msg_qbytes; /* msglen_t */
    int32_t msg_lspid;   /* pid_t */
    int32_t msg_lrpid;
    uint64_t __reserved4;
    uint64_t __reserved5;
};

/* The message header every msgsnd/msgrcv payload starts with.  mtype is a
 * `long` on Linux, i.e. 64 bits; see the spelling note in the header. */
struct msgbuf {
    int64_t mtype;
    char mtext[1];
};

/* msgsnd/msgrcv flags */
#define MSG_NOERROR 010000
#define MSG_EXCEPT 020000
#define MSG_COPY 040000

/* msgctl commands beyond the IPC_* set */
#define MSG_STAT 11
#define MSG_INFO 12
#define MSG_STAT_ANY 13

/*
 * REFUSAL, everything from here down, and uniformly ENOSYS.
 *
 * ENOSYS rather than a more specific errno is the accurate answer: the failure
 * is not that a key is missing (ENOENT), that a table is full (ENOSPC) or that
 * permission was denied (EACCES) -- it is that this host has no System V IPC
 * namespace for the call to fail *within*.  A guest handed ENOENT would retry
 * with IPC_CREAT; handed ENOSPC it would wait for a slot; ENOSYS is the one
 * answer that does not send it looking for a resource that will never appear.
 *
 * ftok() is included in the refusal even though it is pure arithmetic on a
 * stat() result and could technically compute something.  A key it returned
 * would be a key for a namespace that does not exist, and the only thing a
 * caller does with one is pass it to shmget/semget/msgget below -- so producing
 * it would just move the failure one call later and make it look like a lookup
 * miss.  (Its documented failure return is (key_t)-1.)
 */
static inline key_t ftok(const char *path, int identifier) {
    (void)path;
    (void)identifier;
    errno = ENOSYS;
    return (key_t)-1;
}

static inline int shmget(key_t key, size_t size, int flags) {
    (void)key;
    (void)size;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

/* shmat's failure value is (void *)-1, not NULL -- the same convention as
 * MAP_FAILED, and for the same reason: NULL is a legal attach address to ask
 * for. */
static inline void *shmat(int identifier, const void *address, int flags) {
    (void)identifier;
    (void)address;
    (void)flags;
    errno = ENOSYS;
    return (void *)-1;
}

static inline int shmdt(const void *address) {
    (void)address;
    errno = ENOSYS;
    return -1;
}

static inline int shmctl(int identifier, int command, struct shmid_ds *buffer) {
    (void)identifier;
    (void)command;
    (void)buffer;
    errno = ENOSYS;
    return -1;
}

static inline int semget(key_t key, int count, int flags) {
    (void)key;
    (void)count;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline int semop(int identifier, struct sembuf *operations, size_t count) {
    (void)identifier;
    (void)operations;
    (void)count;
    errno = ENOSYS;
    return -1;
}

static inline int semtimedop(int identifier, struct sembuf *operations, size_t count, const struct timespec *timeout) {
    (void)identifier;
    (void)operations;
    (void)count;
    (void)timeout;
    errno = ENOSYS;
    return -1;
}

/* Variadic to match every host's declaration: the fourth argument is the
 * caller's own `union semun` and its presence depends on the command. */
static inline int semctl(int identifier, int number, int command, ...) {
    (void)identifier;
    (void)number;
    (void)command;
    errno = ENOSYS;
    return -1;
}

static inline int msgget(key_t key, int flags) {
    (void)key;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline int msgsnd(int identifier, const void *message, size_t size, int flags) {
    (void)identifier;
    (void)message;
    (void)size;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline ssize_t msgrcv(int identifier, void *message, size_t size, long type, int flags) {
    (void)identifier;
    (void)message;
    (void)size;
    (void)type;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline int msgctl(int identifier, int command, struct msqid_ds *buffer) {
    (void)identifier;
    (void)command;
    (void)buffer;
    errno = ENOSYS;
    return -1;
}

#endif /* _WIN32 */

#endif
