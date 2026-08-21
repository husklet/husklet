// Guest driver for the near-native performance floor benchmark.
//
// Three phases, chosen so that each cost is a slope or a per-operation constant
// rather than a wall-clock ratio, and so that no counter has to be attributed
// across `fork`:
//
//   spawn N K   fork + execve(self "child" K) + wait4, N times. The reported
//               microseconds are wall clock measured in the parent around the
//               whole loop, so every child's cost is inside the window by
//               construction. There are no per-process counters to misattribute.
//   child K     K raw getpid syscalls, then _exit. Subtracting `spawn N 0` from
//               `spawn N K` leaves N*K crossings and nothing else, so the
//               per-crossing cost is a slope.
//   image N P   fork + execve(P) + wait4, N times, where P is a DYNAMICALLY linked
//               image. The exec path walks /proc/self/fd once per image, and a
//               dynamic guest carries a second image -- its PT_INTERP loader --
//               so a change to that walk shows here and barely shows in `spawn`.
//   spin S      Pure guest arithmetic. No fork, no execve, no syscall inside the
//               timed region. This is the control phase for any change to the
//               host-side exec path: such a change cannot reach it.
//
// Output framing matches the repository benchmark convention exactly:
//   PHASE <name> us=<microseconds> ok=<work-proof>
#include <stddef.h>
#include <stdint.h>

#include <errno.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long long micros(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (long long)now.tv_sec * 1000000LL + now.tv_nsec / 1000;
}

static void put(const char *text) {
    size_t length = 0;
    while (text[length] != '\0') {
        length++;
    }
    ssize_t written = 0;
    while ((size_t)written < length) {
        ssize_t step = write(1, text + written, length - (size_t)written);
        if (step <= 0) {
            _exit(70);
        }
        written += step;
    }
}

static void put_unsigned(unsigned long long value) {
    char digits[24];
    size_t index = sizeof(digits);
    digits[--index] = '\0';
    do {
        digits[--index] = (char)('0' + (value % 10));
        value /= 10;
    } while (value != 0);
    put(&digits[index]);
}

static void phase(const char *name, long long elapsed, unsigned long long ok) {
    put("PHASE ");
    put(name);
    put(" us=");
    put_unsigned(elapsed < 0 ? 0ULL : (unsigned long long)elapsed);
    put(" ok=");
    put_unsigned(ok);
    put("\n");
}

static unsigned long long parse(const char *text) {
    unsigned long long value = 0;
    for (size_t index = 0; text[index] != '\0'; index++) {
        if (text[index] < '0' || text[index] > '9') {
            _exit(64);
        }
        value = value * 10ULL + (unsigned long long)(text[index] - '0');
    }
    return value;
}

// The guest sees its own image at an engine-chosen path, not at the host path the
// engine was invoked with, so the loop must re-execute the kernel's own answer
// rather than argv[0]. `/proc/self/exe` is correct on the bare host and under the
// engine alike, which keeps both arms executing the identical image.
static int spawn(const char *self, unsigned long long count, const char *syscalls) {
    static char image[4096];
    ssize_t length = readlink("/proc/self/exe", image, sizeof(image) - 1);
    if (length <= 0) {
        return 74;
    }
    image[length] = '\0';
    self = image;
    char *const arguments[] = {(char *)self, (char *)"child", (char *)syscalls, NULL};
    char *const environment[] = {NULL};
    unsigned long long completed = 0;
    long long start = micros();
    for (unsigned long long index = 0; index < count; index++) {
        pid_t child = fork();
        if (child == 0) {
            execve(self, arguments, environment);
            _exit(errno & 0x7f);
        }
        if (child < 0) {
            return 72;
        }
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            return 73;
        }
        completed++;
    }
    phase("spawn", micros() - start, completed);
    return 0;
}

// The victim is named rather than re-executed from /proc/self/exe: this phase exists to
// exec an image the driver itself is not, namely a dynamically linked one. Its argument is
// inert for the images used here (`true` is a busybox applet and an argument /bin/true
// ignores), so the child's own work stays at zero and the phase measures exec alone.
static int image(unsigned long long count, const char *victim) {
    char *const arguments[] = {(char *)victim, (char *)"true", NULL};
    char *const environment[] = {NULL};
    unsigned long long completed = 0;
    long long start = micros();
    for (unsigned long long index = 0; index < count; index++) {
        pid_t child_pid = fork();
        if (child_pid == 0) {
            execve(victim, arguments, environment);
            _exit(errno & 0x7f);
        }
        if (child_pid < 0) {
            return 72;
        }
        int status = 0;
        if (waitpid(child_pid, &status, 0) != child_pid || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            return 73;
        }
        completed++;
    }
    phase("image", micros() - start, completed);
    return 0;
}

static int child(unsigned long long syscalls) {
    for (unsigned long long index = 0; index < syscalls; index++) {
        syscall(SYS_getpid);
    }
    return 0;
}

static int spin(unsigned long long iterations) {
    unsigned long long accumulator = 12345ULL;
    long long start = micros();
    for (unsigned long long index = 0; index < iterations; index++) {
        accumulator = accumulator * 6364136223846793005ULL + 1442695040888963407ULL;
        accumulator ^= accumulator >> 29;
    }
    long long elapsed = micros() - start;
    phase("spin", elapsed, accumulator & 0xffffffULL);
    return 0;
}

int main(int count, char **arguments) {
    if (count < 3) {
        put("usage: floor spawn <count> <syscalls> | floor image <count> <path> | floor child <syscalls> | floor spin <iterations>\n");
        return 64;
    }
    const char *mode = arguments[1];
    if (mode[0] == 's' && mode[1] == 'p' && mode[2] == 'a') {
        if (count < 4) {
            return 64;
        }
        return spawn(arguments[0], parse(arguments[2]), arguments[3]);
    }
    if (mode[0] == 'i') {
        if (count < 4) {
            return 64;
        }
        return image(parse(arguments[2]), arguments[3]);
    }
    if (mode[0] == 'c') {
        return child(parse(arguments[2]));
    }
    if (mode[0] == 's' && mode[1] == 'p' && mode[2] == 'i') {
        return spin(parse(arguments[2]));
    }
    return 64;
}
