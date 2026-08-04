#define _GNU_SOURCE
#include <fcntl.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile unsigned char *expected;
static volatile sig_atomic_t metadata;

static void on_bus(int number, siginfo_t *info, void *context) {
    (void)number;
    (void)context;
    if (info && info->si_code == BUS_ADRERR && info->si_addr == (void *)expected) metadata++;
    siglongjmp(jump, 1);
}

static int bus_load(volatile unsigned char *address) {
    expected = address;
    if (sigsetjmp(jump, 1) == 0) { (void)*address; return 0; }
    return 1;
}

static int bus_store(volatile unsigned char *address) {
    expected = address;
    if (sigsetjmp(jump, 1) == 0) { *address = 1; return 0; }
    return 1;
}

int main(void) {
    const char path[] = "/data";
    unsigned char data[12288], marker = 0x6d;
    struct sigaction action = {0};
    int command[2], reply[2], fd = open(path, O_RDWR);
    memset(data, 0x2a, sizeof data);
    if (fd < 0 || pwrite(fd, data, sizeof data, 0) != (ssize_t)sizeof data || pipe(command) || pipe(reply)) return 1;
    unsigned char *shared = mmap(NULL, 16384, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *private = mmap(NULL, 16384, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (shared == MAP_FAILED || private == MAP_FAILED) return 2;
    private[31] = 0x91;
    action.sa_sigaction = on_bus;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL)) return 3;

    pid_t peer = fork();
    if (peer == 0) {
        char op;
        close(command[1]); close(reply[0]); close(fd);
        int external = open(path, O_RDWR);
        if (external < 0 || read(command[0], &op, 1) != 1 || ftruncate(external, 5000) != 0 ||
            write(reply[1], "s", 1) != 1 || read(command[0], &op, 1) != 1 ||
            ftruncate(external, 12288) != 0 || pwrite(external, &marker, 1, 8192) != 1 ||
            write(reply[1], "e", 1) != 1)
            _exit(20);
        close(external);
        _exit(0);
    }
    close(command[0]); close(reply[1]);
    char byte = 's';
    if (peer < 0 || write(command[1], &byte, 1) != 1 || read(reply[0], &byte, 1) != 1) return 4;
    int partial = shared[4999] == 0x2a && shared[5000] == 0 && private[5000] == 0;
    int shrink_shared = bus_load(shared + 8192);
    int shrink_private = bus_store(private + 8192);
    byte = 'e';
    if (write(command[1], &byte, 1) != 1 || read(reply[0], &byte, 1) != 1) return 5;
    volatile unsigned int spins = 0;
    while (private[8192] != marker && spins++ != 100000000u) {}
    int extend_shared = shared[8192] == marker;
    int extend_private = private[8192] == marker;
    int dirty_private = private[31] == 0x91;
    int status = 0;
    if (waitpid(peer, &status, 0) != peer) return 6;
    int peer_ok = WIFEXITED(status) && WEXITSTATUS(status) == 0;
    munmap(shared, 16384);
    munmap(private, 16384);
    if (ftruncate(fd, 4096) != 0) return 8;
    unsigned char *short_map = mmap(NULL, 12288, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (short_map == MAP_FAILED) return 9;
    short_map[17] = 0x83;
    int reopened = open(path, O_RDWR);
    marker = 0x74;
    if (reopened < 0 || ftruncate(reopened, 12288) != 0 || pwrite(reopened, &marker, 1, 8192) != 1) return 10;
    int initial_extend = short_map[8192] == marker && short_map[17] == 0x83;
    close(reopened);
    munmap(short_map, 12288);
    close(fd);
    printf("truncate-peer partial=%d shrink-shared=%d shrink-private=%d extend-shared=%d extend-private=%d dirty-private=%d initial-extend=%d metadata=%d peer=%d\n",
           partial, shrink_shared, shrink_private, extend_shared, extend_private, dirty_private, initial_extend,
           metadata == 2, peer_ok);
    return partial && shrink_shared && shrink_private && extend_shared && extend_private && dirty_private &&
                   initial_extend && metadata == 2 && peer_ok
               ? 0
               : 7;
}
