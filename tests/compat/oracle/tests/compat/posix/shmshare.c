// POSIX shared memory: shm_open + ftruncate + mmap(MAP_SHARED) visible across fork.
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdatomic.h>

#define SZ 4096

int main(void) {
    char name[64];
    snprintf(name, sizeof name, "/hl_shm_%d", (int)getpid());
    shm_unlink(name);
    int fd = shm_open(name, O_CREAT | O_RDWR | O_EXCL, 0600);
    if (fd < 0) { printf("shmshare open=0\n"); return 0; }
    int trunc = ftruncate(fd, SZ) == 0;

    atomic_int *shared = mmap(NULL, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED) { printf("shmshare mmap=0\n"); close(fd); shm_unlink(name); return 0; }
    atomic_store(&shared[0], 100);
    atomic_store(&shared[1], 0);

    pid_t pid = fork();
    if (pid == 0) {
        // Child sees the parent's write and writes back.
        int seen = atomic_load(&shared[0]) == 100;
        atomic_store(&shared[1], seen ? 200 : -1);
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    int roundtrip = atomic_load(&shared[1]) == 200;

    // A second open of the same name maps the same object.
    int fd2 = shm_open(name, O_RDWR, 0600);
    atomic_int *m2 = mmap(NULL, SZ, PROT_READ, MAP_SHARED, fd2, 0);
    int alias = m2 != MAP_FAILED && atomic_load(&m2[1]) == 200;

    munmap(shared, SZ);
    if (m2 != MAP_FAILED) munmap(m2, SZ);
    close(fd);
    close(fd2);
    int unlinked = shm_unlink(name) == 0;
    printf("shmshare trunc=%d roundtrip=%d alias=%d unlink=%d\n",
           trunc, roundtrip, alias, unlinked);
    return 0;
}
