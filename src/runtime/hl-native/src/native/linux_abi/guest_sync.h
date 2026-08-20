#ifndef HL_LINUX_ABI_GUEST_SYNC_H
#define HL_LINUX_ABI_GUEST_SYNC_H

/*
 * Synchronised-I/O open flags: the guest's O_DSYNC / O_SYNC <-> the host's.
 *
 * These two bits are a DURABILITY CONTRACT, not a hint. Under O_DSYNC every
 * write(2) must have reached the storage device before it returns; O_SYNC adds
 * the inode metadata. Dropping them turns an acknowledged barrier into an
 * ordinary page-cache write, which is silent data loss on the next host crash
 * -- and PostgreSQL's default wal_sync_method on Linux is `open_datasync`,
 * which opens the WAL O_DSYNC and then never calls fdatasync on it, so the
 * whole commit durability chain hangs off this one translation.
 *
 * Linux's values are arch-independent (include/uapi/asm-generic/fcntl.h):
 *   O_DSYNC   0x1000
 *   __O_SYNC  0x100000
 *   O_SYNC    (__O_SYNC | O_DSYNC) == 0x101000
 * and fs/fcntl.c forces O_DSYNC on whenever __O_SYNC is set, so __O_SYNC alone
 * still means the STRONGER barrier. The mapping below preserves that ordering:
 * a guest asking for O_SYNC must never land on the weaker host flag.
 */

#define HL_GUEST_O_DSYNC 0x1000
#define HL_GUEST_O_FULL_SYNC 0x100000 /* Linux __O_SYNC */
#define HL_GUEST_O_SYNC (HL_GUEST_O_FULL_SYNC | HL_GUEST_O_DSYNC)

#if (defined(__linux__) || defined(__APPLE__)) && defined(O_DSYNC) && defined(O_SYNC)
#define HL_HOST_HAS_SYNC_OPEN 1
#else
/* Any host whose open(2) cannot express the barrier at all. Nothing is mapped
 * rather than something weaker being mapped and read as success. */
#define HL_HOST_HAS_SYNC_OPEN 0
#endif

/* Host open(2) bits to OR in for a guest open-flag word. 0 when the guest asked
 * for neither barrier (or the host cannot express one). */
static inline int hl_guest_sync_open_flags(int guest_flags) {
#if HL_HOST_HAS_SYNC_OPEN
    if (guest_flags & HL_GUEST_O_FULL_SYNC) return O_SYNC;
    if (guest_flags & HL_GUEST_O_DSYNC) return O_DSYNC;
#else
    (void)guest_flags;
#endif
    return 0;
}

/* The reverse direction, for F_GETFL: guest status bits for a host flag word,
 * so an fd opened O_DSYNC reads back as O_DSYNC instead of a plain fd. */
static inline int hl_host_sync_guest_flags(int host_flags) {
#if HL_HOST_HAS_SYNC_OPEN
    if ((host_flags & O_SYNC) == O_SYNC) return HL_GUEST_O_SYNC;
    if (host_flags & O_DSYNC) return HL_GUEST_O_DSYNC;
#else
    (void)host_flags;
#endif
    return 0;
}

#endif
