// A NONBLOCKING read on a controlling terminal with no pending input returns EAGAIN on Linux, NEVER EOF(0):
// a tty has no end-of-input from emptiness. hl backs /dev/tty with a host device (and /dev/console with
// /dev/null) that returns 0 when empty, so readline/TUI/event-loop code read the 0 as terminal closure and
// tear the terminal down. The fix flags /dev/tty + /dev/console fds and remaps a nonblocking 0-byte read to
// EAGAIN.
//
// This is a hl-behavior golden (not an oracle diff): a real controlling tty is not guaranteed on the test
// host, and native /dev/console read semantics inside a container are not portable. /dev/console is the
// deterministic path -- hl intercepts open("/dev/console") unconditionally (backed by /dev/null), so it
// exercises the exact flag+read fix here. /dev/tty is attempted best-effort (it needs the engine to hold a
// controlling terminal, which the compiled-guest harness lacks); when it can't be opened it is skipped.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

// Open `path` nonblocking, read once. Returns: 1 => EAGAIN (correct tty semantics), 0 => EOF/0-byte read
// (the bug), -1 => could not open (skip), -2 => some other read error.
static int probe(const char *path) {
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    if (fd < 0) return -1;
    char b[16];
    errno = 0;
    ssize_t r = read(fd, b, sizeof b);
    int e = errno;
    close(fd);
    if (r < 0) return (e == EAGAIN || e == EWOULDBLOCK) ? 1 : -2;
    return 0; // r >= 0 with no error: a 0-byte read here is the EOF-instead-of-EAGAIN bug
}

int main(void) {
    // Deterministic path: /dev/console is always intercepted by hl (backed by /dev/null) -> open succeeds,
    // and a nonblocking empty read MUST be EAGAIN, not EOF.
    int console = probe("/dev/console");

    // Best-effort: /dev/tty needs a controlling terminal the harness engine may not have. If openable it
    // must ALSO give EAGAIN, never a 0-byte EOF.
    int tty = probe("/dev/tty");

    // ok iff the console path returned EAGAIN, and /dev/tty (when openable) did too (skip == openable-fail).
    int ok = (console == 1) && (tty == 1 || tty == -1);
    printf("ttynonblock ok=%d console=%d tty=%d\n", ok, console, tty);
    return 0;
}
