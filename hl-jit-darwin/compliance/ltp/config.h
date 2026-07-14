/* Hand-built config.h for the hl LTP compliance lane (no autotools).
 * Targets a modern Linux glibc (>=2.36) + recent kernel UAPI headers, as on the
 * cross toolchain host. Each HAVE_ below mirrors what LTP's ./configure would
 * detect on such a system; they suppress LTP's lapi/ fallback re-declarations
 * that would otherwise clash with the now-standard glibc/UAPI definitions.
 * Feature macros left undefined default OFF (their code compiles out) — safe. */
#ifndef LTP_CONFIG_H__
#define LTP_CONFIG_H__
#define HAVE_ATOMIC_MEMORY_MODEL 1
#define HAVE_SYNC_ADD_AND_FETCH 1
#define HAVE_UTIMENSAT 1
#define HAVE_SYS_RANDOM_H 1
#define HAVE_STRUCT_TERMIO 1
#define HAVE_IPC64_PERM 1
#define HAVE_SEMID64_DS 1
#define HAVE_MSQID64_DS 1
#define HAVE_SHMID64_DS 1
#define HAVE_DECL_MADV_MERGEABLE 1
/* glibc-provided wrappers / structs (avoid lapi/ re-declarations) */
#define HAVE_SETNS 1
#define HAVE_GETCPU 1
#define HAVE_FALLOCATE 1
#define HAVE_STRUCT_STATMOUNT 1
#define HAVE_STRUCT_MNT_ID_REQ_MNT_NS_FD 1
#define HAVE_STRUCT_MOUNT_ATTR 1
#define HAVE_FSOPEN 1
#define HAVE_FSCONFIG 1
#define HAVE_FSMOUNT 1
#define HAVE_FSPICK 1
#define HAVE_MOVE_MOUNT 1
#define HAVE_OPEN_TREE 1
#define HAVE_OPEN_TREE_ATTR 1
#define HAVE_MOUNT_SETATTR 1
#define HAVE_STRUCT_STATX 1
#define HAVE_STRUCT_STATX_TIMESTAMP 1
#define HAVE_STATX 1
#define HAVE_PREADV 1
#define HAVE_PWRITEV 1
#define HAVE_PREADV2 1
#define HAVE_PWRITEV2 1
#define HAVE_EPOLL_PWAIT 1
#define HAVE_EPOLL_PWAIT2 1
#define HAVE_STRUCT_STATX 1
#define HAVE_STRUCT_STATX_TIMESTAMP 1
#define HAVE_STATX 1
#define HAVE_PREADV 1
#define HAVE_PWRITEV 1
#define HAVE_PREADV2 1
#define HAVE_PWRITEV2 1
#define HAVE_EPOLL_PWAIT 1
#define HAVE_EPOLL_PWAIT2 1
/* struct file_attr is now provided by modern kernel UAPI <linux/fs.h>, so
 * lapi/fs.h must NOT re-declare it (would be a redefinition). Guards
 * statx04/05/08/09, setxattr03, utimensat01. */
#define HAVE_STRUCT_FILE_ATTR 1
/* glibc's <sys/timerfd.h> provides timerfd_create/settime/gettime and the
 * TFD_* flags (TFD_CLOEXEC, TFD_TIMER_ABSTIME). Tell lapi/timerfd.h to include
 * it and NOT emit its own conflicting fallback wrappers. Guards timerfd01/02/04,
 * timerfd_settime02. */
#define HAVE_SYS_TIMERFD_H 1
#define HAVE_TIMERFD_CREATE 1
#define HAVE_TIMERFD_GETTIME 1
#define HAVE_TIMERFD_SETTIME 1
#endif
