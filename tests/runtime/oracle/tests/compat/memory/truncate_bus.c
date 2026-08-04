#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile unsigned char *fault_address;
static volatile sig_atomic_t metadata_ok;

static void on_bus(int number, siginfo_t *info, void *context) {
    (void)number;
    (void)context;
    if (info != NULL && info->si_code == BUS_ADRERR && info->si_addr == (void *)fault_address) metadata_ok++;
    siglongjmp(jump, 1);
}

static int catches_load(volatile unsigned char *address) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) { (void)*address; return 0; }
    return 1;
}

static int catches_store(volatile unsigned char *address) {
    fault_address = address;
    if (sigsetjmp(jump, 1) == 0) { *address = 0x44; return 0; }
    return 1;
}

int main(void) {
    char path[] = "/tmp/hl-truncate-bus-XXXXXX";
    unsigned char source[12288], value = 0x6d, observed = 0;
    struct sigaction action = {0};
    int fd = mkstemp(path), copy;
    if (fd < 0) return 1;
    memset(source, 0x2a, sizeof source);
    if (write(fd, source, sizeof source) != (ssize_t)sizeof source) return 2;
    copy = dup(fd);
    if (copy < 0) return 3;
    unsigned char *shared = mmap(NULL, 16384, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    unsigned char *private = mmap(NULL, 16384, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0);
    if (shared == MAP_FAILED || private == MAP_FAILED) return 4;
    action.sa_sigaction = on_bus;
    action.sa_flags = SA_SIGINFO;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGBUS, &action, NULL) != 0) return 5;

    if (ftruncate(copy, 5000) != 0) return 6;
    int partial_zero = shared[4999] == 0x2a && shared[5000] == 0 && private[5000] == 0;
    int shared_load = catches_load(shared + 8192);
    int shared_store = catches_store(shared + 8192);
    int private_load = catches_load(private + 8192);
    int private_store = catches_store(private + 8192);

    pid_t child = fork();
    if (child == 0) {
        signal(SIGBUS, SIG_DFL);
        (void)*(volatile unsigned char *)(shared + 12288);
        _exit(9);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child) return 7;
    int fork_bus = WIFSIGNALED(status) && WTERMSIG(status) == SIGBUS;

    if (ftruncate(fd, 12288) != 0 || pwrite(fd, &value, 1, 8192) != 1) return 8;
    int shared_extend = shared[8192] == value;
    int private_extend = private[8192] == value;
    shared[8193] = 0x7c;
    int shared_write = pread(fd, &observed, 1, 8193) == 1 && observed == 0x7c;

    printf("truncate-bus partial=%d shared-load=%d shared-store=%d private-load=%d private-store=%d fork=%d extend-shared=%d extend-private=%d shared-write=%d metadata=%d\n",
           partial_zero, shared_load, shared_store, private_load, private_store, fork_bus, shared_extend,
           private_extend, shared_write, metadata_ok == 4);
    munmap(shared, 16384);
    munmap(private, 16384);
    close(copy);
    close(fd);
    unlink(path);
    return partial_zero && shared_load && shared_store && private_load && private_store && fork_bus &&
                   shared_extend && private_extend && shared_write && metadata_ok == 4
               ? 0
               : 9;
}
