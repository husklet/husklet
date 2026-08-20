/* Engine-owned per-terminal termios.
 *
 * A guest terminal's authoritative `struct termios` is the image the guest last
 * installed, not whatever the host line discipline happens to hold. The two
 * genuinely differ. `termios_l2m`/`termios_m2l` translate only the flags that
 * have a BSD counterpart, so on a macOS host a plain tcsetattr/tcgetattr round
 * trip silently drops
 *
 *   c_iflag  IUCLC, IUTF8
 *   c_oflag  OLCUC, OFILL, OFDEL and every delay field, XTABS included
 *   c_lflag  XCASE, ECHOCTL, ECHOPRT, ECHOKE, FLUSHO, PENDIN, EXTPROC
 *
 * and every shell that runs `stty sane` installs ECHOCTL and ECHOKE, so a guest
 * doing the standard save/modify/restore sequence loses them on the first
 * restore and `stty -a` misreports them from then on. Remembering the guest's
 * own image here makes TCGETS answer with exactly what the guest installed,
 * whatever the host discipline is able to represent.
 *
 * The store never invents a bit the host contradicts. `remember` pairs the
 * guest image with the host projection observed immediately after installing
 * it, and `recall` returns the guest image only while the host still holds that
 * projection. A host-side change, or a recycled device inode landing on a stale
 * entry, therefore reads as a miss and the caller falls back to translating the
 * host's own termios, which is today's behaviour.
 */

#include <pthread.h>
#include <stdatomic.h>
#include <sys/stat.h>

#define TERMINAL_TERMIOS_IMAGE 36
#define TERMINAL_TERMIOS_CAPACITY 16

typedef struct {
    int used;
    uint64_t stamp;
    dev_t device;
    ino_t inode;
    uint8_t image[TERMINAL_TERMIOS_IMAGE];
    uint8_t mirror[TERMINAL_TERMIOS_IMAGE];
} terminal_termios_entry;

static terminal_termios_entry g_terminal_termios[TERMINAL_TERMIOS_CAPACITY];
static pthread_mutex_t g_terminal_termios_lock = PTHREAD_MUTEX_INITIALIZER;
static uint64_t g_terminal_termios_stamp;
/* Bumped on every remember. The engine's terminal pump reads this once per wakeup to decide whether
 * its cached termios is still current, so the common case -- a keystroke arriving with the guest's
 * termios unchanged -- costs one relaxed load and no lock, no fstat and no tcgetattr. */
static _Atomic uint64_t g_terminal_termios_generation;

static int terminal_termios_identity(int native_fd, dev_t *device, ino_t *inode) {
    struct stat status;
    if (native_fd < 0 || fstat(native_fd, &status) != 0) return 0;
    *device = status.st_dev;
    *inode = status.st_ino;
    return 1;
}

/* Answer the guest-authored image for `native_fd`, or 0 when nothing is
 * remembered for it and the caller should keep the host's own translation.
 * `host_image` is the projection the caller just read from the host. */
static int terminal_termios_recall(int native_fd, const uint8_t *host_image, uint8_t *out) {
    dev_t device;
    ino_t inode;
    if (!terminal_termios_identity(native_fd, &device, &inode)) return 0;
    int recalled = 0;
    pthread_mutex_lock(&g_terminal_termios_lock);
    for (int index = 0; index < TERMINAL_TERMIOS_CAPACITY; ++index) {
        terminal_termios_entry *entry = &g_terminal_termios[index];
        if (!entry->used || entry->device != device || entry->inode != inode) continue;
        if (memcmp(entry->mirror, host_image, TERMINAL_TERMIOS_IMAGE) == 0) {
            memcpy(out, entry->image, TERMINAL_TERMIOS_IMAGE);
            recalled = 1;
        } else {
            /* The host moved underneath us, or this inode has been recycled onto
             * a different terminal. Either way the remembered image no longer
             * describes this device. */
            entry->used = 0;
        }
        break;
    }
    pthread_mutex_unlock(&g_terminal_termios_lock);
    return recalled;
}

/* Remember `image` as the guest's view of `native_fd`, paired with the host
 * projection `host_image` that installing it produced. A full table evicts its
 * least recently written entry: losing an entry costs that terminal the flags
 * BSD cannot hold, which is today's behaviour, whereas refusing to record would
 * disable the store for the rest of the process. */
static void terminal_termios_remember(int native_fd, const uint8_t *image, const uint8_t *host_image) {
    dev_t device;
    ino_t inode;
    if (!terminal_termios_identity(native_fd, &device, &inode)) return;
    pthread_mutex_lock(&g_terminal_termios_lock);
    terminal_termios_entry *chosen = NULL;
    for (int index = 0; index < TERMINAL_TERMIOS_CAPACITY; ++index) {
        terminal_termios_entry *entry = &g_terminal_termios[index];
        if (entry->used && entry->device == device && entry->inode == inode) {
            chosen = entry;
            break;
        }
        if (!entry->used) {
            if (chosen == NULL || chosen->used) chosen = entry;
            continue;
        }
        if (chosen == NULL) chosen = entry;
        else if (chosen->used && entry->stamp < chosen->stamp) chosen = entry;
    }
    chosen->used = 1;
    chosen->stamp = ++g_terminal_termios_stamp;
    atomic_fetch_add_explicit(&g_terminal_termios_generation, 1, memory_order_release);
    chosen->device = device;
    chosen->inode = inode;
    memcpy(chosen->image, image, TERMINAL_TERMIOS_IMAGE);
    memcpy(chosen->mirror, host_image, TERMINAL_TERMIOS_IMAGE);
    pthread_mutex_unlock(&g_terminal_termios_lock);
}

/* Read the host termios of `native_fd` as a Linux image. Returns 0 on success
 * and -errno otherwise. On a Linux host the host structure already is the guest
 * ABI, so the translation tables are bypassed entirely. */
static int terminal_termios_host_image(int native_fd, uint8_t *out) {
    struct termios native;
    if (tcgetattr(native_fd, &native) != 0) return -errno;
#if defined(__linux__)
    memset(out, 0, TERMINAL_TERMIOS_IMAGE);
    memcpy(out, &native, TERMINAL_TERMIOS_IMAGE);
#else
    termios_m2l(&native, out);
#endif
    return 0;
}

/* Record the outcome of a guest TCSETS: re-read what the host actually kept and
 * pair it with the image the guest asked for. */
static void terminal_termios_observe_set(int native_fd, const uint8_t *image) {
    uint8_t host_image[TERMINAL_TERMIOS_IMAGE];
    if (terminal_termios_host_image(native_fd, host_image) == 0)
        terminal_termios_remember(native_fd, image, host_image);
}

/* Overwrite `argument`'s first 36 bytes with the guest-authored image when one
 * is remembered for this terminal. `argument` must already hold the host's own
 * translation, which is what stays in place on a miss. */
static void terminal_termios_apply_recall(int native_fd, uint8_t *argument) {
    uint8_t recalled[TERMINAL_TERMIOS_IMAGE];
    if (terminal_termios_recall(native_fd, argument, recalled))
        memcpy(argument, recalled, TERMINAL_TERMIOS_IMAGE);
}

/* How many times any terminal's guest image has been installed. A reader that sees an unchanged value
 * may keep whatever image it last read. */
HL_API uint64_t HL_TARGET_LOCAL(terminal_termios_generation)(void) {
    return atomic_load_explicit(&g_terminal_termios_generation, memory_order_acquire);
}

/* The guest-authored image for the terminal `native_fd` names, or 0 when this ISA's store has none.
 *
 * Unlike `terminal_termios_recall` this does NOT require the host to still hold the projection that
 * installing the image produced. The engine's terminal pump is the one party that deliberately makes
 * the host diverge -- it puts the host slave in raw mode so the Linux line discipline can run over a
 * channel that does not flush -- so for it a mismatch is the expected state rather than evidence of a
 * stale entry. Every other caller wants `terminal_termios_recall` and its guard. */
HL_API int HL_TARGET_LOCAL(terminal_termios_image)(int32_t native_fd, uint8_t *out) {
    dev_t device;
    ino_t inode;
    if (out == NULL || !terminal_termios_identity((int)native_fd, &device, &inode)) return 0;
    int found = 0;
    pthread_mutex_lock(&g_terminal_termios_lock);
    for (int index = 0; index < TERMINAL_TERMIOS_CAPACITY; ++index) {
        terminal_termios_entry *entry = &g_terminal_termios[index];
        if (!entry->used || entry->device != device || entry->inode != inode) continue;
        memcpy(out, entry->image, TERMINAL_TERMIOS_IMAGE);
        found = 1;
        break;
    }
    pthread_mutex_unlock(&g_terminal_termios_lock);
    return found;
}

/* Read the host's own termios for `native_fd` as a Linux image, so a caller outside this translation
 * unit can record what the host held before it deliberately changed it. Returns 0 on failure. */
HL_API int HL_TARGET_LOCAL(terminal_termios_capture)(int32_t native_fd, uint8_t *out) {
    if (out == NULL) return 0;
    return terminal_termios_host_image((int)native_fd, out) == 0 ? 1 : 0;
}

/* Adopt `image` as the guest view of `native_fd`, paired with the host projection as it stands now.
 *
 * This is the engine terminal pump's entry. The pump puts the host slave in raw mode so the Linux
 * line discipline can run over a channel that applies backpressure instead of flushing at
 * `MAX_CANON`, which makes the host disagree with the guest on purpose and would otherwise read as a
 * stale entry to `terminal_termios_recall`. Re-pairing the guest image with the raw projection is
 * what keeps the guest's own TCGETS answering with what the guest installed rather than with the raw
 * mode the pump imposed. Returns 0 when the host termios could not be read, in which case nothing is
 * recorded and the terminal keeps whatever view it already had. */
HL_API int HL_TARGET_LOCAL(terminal_termios_adopt)(int32_t native_fd, const uint8_t *image) {
    uint8_t host_image[TERMINAL_TERMIOS_IMAGE];
    if (image == NULL || terminal_termios_host_image((int)native_fd, host_image) != 0) return 0;
    terminal_termios_remember((int)native_fd, image, host_image);
    return 1;
}

/* Install `image` as the guest view of the terminal `fd` names, so a caller outside this translation
 * unit can then read it back through the bridge and check the whole path. The host projection is
 * recorded as the image itself, which is what a Linux host produces anyway. */
HL_API void HL_TARGET_LOCAL(terminal_termios_install_test)(int32_t fd, const uint8_t *image) {
    terminal_termios_remember((int)fd, image, image);
}

/* Exercise the store without a terminal. Identity comes from fstat, so any pair
 * of distinct descriptors stands in for two terminals and the check runs the
 * same way on every host. Returns 0 on success or the number of the step that
 * failed. */
HL_API int HL_TARGET_LOCAL(terminal_termios_store_test)(void) {
    int first[2], second[2];
    if (pipe(first) != 0) return 1;
    if (pipe(second) != 0) {
        close(first[0]);
        close(first[1]);
        return 2;
    }
    int failure = 0;
    int shared = -1;
    uint8_t host_image[TERMINAL_TERMIOS_IMAGE];
    uint8_t guest_image[TERMINAL_TERMIOS_IMAGE];
    uint8_t moved_image[TERMINAL_TERMIOS_IMAGE];
    uint8_t recalled[TERMINAL_TERMIOS_IMAGE];
    memset(host_image, 0, sizeof host_image);
    /* A plausible cooked host projection, and the guest image that produced it
     * carrying the three lflag bits and the tab delay a BSD termios cannot
     * hold: ECHOCTL, ECHOKE, EXTPROC and XTABS. */
    host_image[12] = 0x3b; /* ISIG|ICANON|ECHO|ECHOE|ECHOK */
    memcpy(guest_image, host_image, sizeof guest_image);
    guest_image[13] = 0x0a;  /* ECHOCTL 0x200 | ECHOKE 0x800 */
    guest_image[14] = 0x01;  /* EXTPROC 0x10000 */
    guest_image[5] = 0x18;   /* XTABS 0x1800 in c_oflag */
    memcpy(moved_image, host_image, sizeof moved_image);
    moved_image[12] = 0x39; /* the host dropped ICANON behind our back */

    do {
        terminal_termios_remember(first[0], guest_image, host_image);
        if (!terminal_termios_recall(first[0], host_image, recalled)) {
            failure = 3;
            break;
        }
        if (memcmp(recalled, guest_image, sizeof recalled) != 0) {
            failure = 4;
            break;
        }
        /* A descriptor onto the same object shares the remembered view. */
        shared = dup(first[0]);
        if (shared < 0) {
            failure = 5;
            break;
        }
        if (!terminal_termios_recall(shared, host_image, recalled) ||
            memcmp(recalled, guest_image, sizeof recalled) != 0) {
            failure = 6;
            break;
        }
        /* An unrelated terminal must not see it. */
        if (terminal_termios_recall(second[0], host_image, recalled)) {
            failure = 7;
            break;
        }
        /* A host that has moved underneath the entry reads as a miss, and the
         * stale entry is dropped rather than resurrected by a later matching
         * projection. */
        if (terminal_termios_recall(first[0], moved_image, recalled)) {
            failure = 8;
            break;
        }
        if (terminal_termios_recall(first[0], host_image, recalled)) {
            failure = 9;
            break;
        }
        /* Re-installing restores the view. */
        terminal_termios_remember(first[0], guest_image, host_image);
        if (!terminal_termios_recall(first[0], host_image, recalled) ||
            memcmp(recalled, guest_image, sizeof recalled) != 0) {
            failure = 10;
            break;
        }
        /* Overflowing the table must degrade to a miss for the terminals it
         * evicts, never to another terminal's image, and the most recently
         * written terminal must survive. */
        int overflow[TERMINAL_TERMIOS_CAPACITY * 2][2];
        int opened = 0;
        while (opened < TERMINAL_TERMIOS_CAPACITY * 2 && pipe(overflow[opened]) == 0) {
            uint8_t distinct[TERMINAL_TERMIOS_IMAGE];
            memcpy(distinct, guest_image, sizeof distinct);
            distinct[16] = (uint8_t)(opened + 1);
            terminal_termios_remember(overflow[opened][0], distinct, host_image);
            opened += 1;
        }
        if (opened < TERMINAL_TERMIOS_CAPACITY + 1) failure = 11;
        for (int index = 0; index < opened; ++index) {
            uint8_t distinct[TERMINAL_TERMIOS_IMAGE];
            memcpy(distinct, guest_image, sizeof distinct);
            distinct[16] = (uint8_t)(index + 1);
            int hit = terminal_termios_recall(overflow[index][0], host_image, recalled);
            if (hit && memcmp(recalled, distinct, sizeof recalled) != 0) failure = 12;
            if (!hit && index == opened - 1) failure = 13;
        }
        for (int index = 0; index < opened; ++index) {
            close(overflow[index][0]);
            close(overflow[index][1]);
        }
    } while (0);

    if (shared >= 0) close(shared);
    close(first[0]);
    close(first[1]);
    close(second[0]);
    close(second[1]);
    return failure;
}
