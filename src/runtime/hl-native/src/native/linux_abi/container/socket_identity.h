#ifndef HL_LINUX_ABI_CONTAINER_SOCKET_IDENTITY_H
#define HL_LINUX_ABI_CONTAINER_SOCKET_IDENTITY_H

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
/* Host CSPRNG, split exactly as `engine/arena.c` splits it. The bare `#else` this
   replaces sent Windows to <sys/random.h>, which does not exist there. */
#if defined(_WIN32)
#include <windows.h>
#include <bcrypt.h>
#elif defined(__APPLE__)
#include <stdlib.h>
#else
#include <sys/random.h>
#endif

/* Reciprocal AF_UNIX connection identity travels in the client's bind name, so that accept() can recover
 * it with getpeername() without putting engine-private bytes in the guest stream.  Two rules make that
 * transport safe and both are load bearing:
 *
 *   1. The name lives under an ENGINE-OWNED directory created mode 0700 (sock_identity_directory in
 *      netns/loopback.c), which no guest path translation can reach.  It is not /tmp itself.
 *   2. The name carries a 128-bit RANDOM ticket nonce -- never the object ids.  Identity is resolved by
 *      claiming that nonce, one shot, out of an engine-private shared ticket table at accept.  Nothing
 *      parsed out of the filename is ever adopted as identity.
 *
 * The earlier encoding wrote the object ids themselves -- (pid << 32) | sequence, therefore predictable --
 * into a world-writable /tmp/.hl-ci-<client>-<server> name and adopted whatever accept() parsed back out.
 * A guest that pre-created a matching name chose the engine's connection identity, and in bare mode a
 * guest AF_UNIX pathname bind passes through to the literal host path, so it could.  That is exactly what
 * `connected-unix-first-safe-boundary` forbids: do not authenticate by parsing a guest-creatable filename
 * prefix.  Once restore topology keys on these object ids, a forged one steers reconnection.  Do not
 * reintroduce that shape.
 */

#define HL_SOCKET_IDENTITY_PREFIX "/tmp/.hl-ci-"
#define HL_SOCKET_IDENTITY_NONCE_DIGITS 32u
/* directory (<= 52) + '/' + 32 nonce digits + NUL, comfortably inside sun_path[108]. */
#define HL_SOCKET_IDENTITY_PATH_SIZE 96

/* Fill a 128-bit ticket nonce from the host CSPRNG.  A failure must fail the connection closed rather
 * than fall back to anything derivable, which is the whole point of the change. */
static inline int hl_socket_identity_nonce_new(uint64_t *high, uint64_t *low) {
    if (!high || !low) return -1;
    uint64_t words[2] = {0, 0};
    do {
#if defined(_WIN32)
        if (BCryptGenRandom(NULL, (PUCHAR)words, (ULONG)sizeof words, BCRYPT_USE_SYSTEM_PREFERRED_RNG) != 0) return -1;
#elif defined(__APPLE__)
        arc4random_buf(words, sizeof words);
#else
        size_t offset = 0;
        while (offset < sizeof words) {
            ssize_t count = getrandom((unsigned char *)words + offset, sizeof words - offset, 0);
            if (count > 0) {
                offset += (size_t)count;
                continue;
            }
            if (count < 0 && errno == EINTR) continue;
            return -1;
        }
#endif
    } while (!words[0] && !words[1]); // 0/0 is the table's "no ticket" sentinel
    *high = words[0];
    *low = words[1];
    return 0;
}

static inline int hl_socket_identity_format(char *path, size_t capacity, const char *directory, uint64_t high,
                                            uint64_t low) {
    if (!path || !directory || !directory[0] || (!high && !low)) return -1;
    size_t directory_length = strlen(directory);
    int length =
        snprintf(path, capacity, "%s/%016llx%016llx", directory, (unsigned long long)high, (unsigned long long)low);
    return length > 0 && (size_t)length < capacity &&
                   (size_t)length == directory_length + 1u + HL_SOCKET_IDENTITY_NONCE_DIGITS
               ? 0
               : -1;
}

/* Recover the ticket nonce a peer bind name carries.  Strict: the name must sit directly beneath the
 * engine's own directory and be exactly 32 lowercase hex digits.  This is a lookup key, not a credential --
 * the caller still has to claim the nonce out of the private table before any identity exists. */
static inline int hl_socket_identity_nonce_parse(const char *path, const char *directory, uint64_t *high,
                                                 uint64_t *low) {
    if (!path || !directory || !directory[0] || !high || !low) return -1;
    size_t directory_length = strlen(directory);
    if (strncmp(path, directory, directory_length) != 0 || path[directory_length] != '/' ||
        strlen(path + directory_length + 1) != HL_SOCKET_IDENTITY_NONCE_DIGITS)
        return -1;
    const char *digits = path + directory_length + 1;
    uint64_t parsed[2] = {0, 0};
    for (size_t index = 0; index < HL_SOCKET_IDENTITY_NONCE_DIGITS; index++) {
        char digit = digits[index];
        unsigned value;
        if (digit >= '0' && digit <= '9')
            value = (unsigned)(digit - '0');
        else if (digit >= 'a' && digit <= 'f')
            value = (unsigned)(digit - 'a') + 10u;
        else
            return -1;
        parsed[index / 16u] = (parsed[index / 16u] << 4) | value;
    }
    if (!parsed[0] && !parsed[1]) return -1;
    *high = parsed[0];
    *low = parsed[1];
    return 0;
}

/* Defense in depth for bare mode, where an unrouted guest AF_UNIX pathname bind reaches the literal host
 * path: refuse any guest bind that aims at the engine's identity namespace. */
static inline int hl_socket_identity_path_reserved(const char *guest_path) {
    return guest_path != NULL &&
           strncmp(guest_path, HL_SOCKET_IDENTITY_PREFIX, sizeof(HL_SOCKET_IDENTITY_PREFIX) - 1) == 0;
}

#endif
