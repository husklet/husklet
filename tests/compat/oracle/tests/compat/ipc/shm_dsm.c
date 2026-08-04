// POSIX /dev/shm functional contract, the shape real software (postgres DSM / parallel workers) needs:
//   (a) ROUND-TRIP+PERSIST: shm_open(O_CREAT)+ftruncate+mmap, write a pattern, munmap+close, then re-open
//       the SAME name, mmap, and verify the pattern survived (the segment persists for the container life).
//   (b) FORK-SHARED DSM (the postgres pattern): parent creates+maps a MAP_SHARED segment, forks; the child
//       opens the SAME name, maps it, and writes a value the parent then reads back -- proving the mapping
//       is coherent across processes, not a private copy.
//   (c) NAMED SEM across fork: sem_open a named semaphore, child sem_post, parent sem_wait unblocks.
// Portable POSIX -> runs emulated-on-Linux AND native-on-macOS (all engines), golden verdict.
#include <fcntl.h>
#include <semaphore.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#define SZ 4096

int main(void) {
    const char *nm = "/hl_shm_dsm";
    const char *sn = "/hl_shm_dsm_sem";
    shm_unlink(nm);
    sem_unlink(sn);

    // (a) round-trip + persist across close/reopen
    int roundtrip = 0;
    {
        int fd = shm_open(nm, O_CREAT | O_RDWR, 0600);
        if (fd >= 0 && ftruncate(fd, SZ) == 0) {
            char *p = mmap(0, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
            if (p != MAP_FAILED) {
                memset(p, 0xA5, SZ);
                p[0] = 'D'; p[1] = 'S'; p[2] = 'M';
                munmap(p, SZ);
            }
            close(fd);
        }
        int fd2 = shm_open(nm, O_RDWR, 0600);
        if (fd2 >= 0) {
            char *q = mmap(0, SZ, PROT_READ, MAP_SHARED, fd2, 0);
            if (q != MAP_FAILED) {
                roundtrip = (q[0] == 'D' && q[1] == 'S' && q[2] == 'M' &&
                             (unsigned char)q[100] == 0xA5 && (unsigned char)q[SZ - 1] == 0xA5);
                munmap(q, SZ);
            }
            close(fd2);
        }
    }

    // (b) fork-shared DSM: parent maps, child opens+maps, writes; parent reads it back
    int forkshared = 0;
    {
        int fd = shm_open(nm, O_RDWR, 0600);
        volatile long *ctr = mmap(0, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        ctr[10] = 111;
        pid_t pid = fork();
        if (pid == 0) {
            int cfd = shm_open(nm, O_RDWR, 0600);
            volatile long *cc = mmap(0, SZ, PROT_READ | PROT_WRITE, MAP_SHARED, cfd, 0);
            cc[10] = cc[10] + 888; // 111 -> 999, seen by parent through the shared page
            munmap((void *)cc, SZ);
            close(cfd);
            _exit(0);
        }
        waitpid(pid, 0, 0);
        forkshared = (ctr[10] == 999);
        munmap((void *)ctr, SZ);
        close(fd);
    }

    // (c) named POSIX semaphore across fork
    int semok = 0;
    {
        sem_t *s = sem_open(sn, O_CREAT, 0600, 0);
        if (s != SEM_FAILED) {
            pid_t pid = fork();
            if (pid == 0) { sem_post(s); _exit(0); }
            waitpid(pid, 0, 0);
            semok = (sem_wait(s) == 0);
            sem_close(s);
        }
    }

    shm_unlink(nm);
    sem_unlink(sn);
    printf("shm_dsm roundtrip=%d forkshared=%d sem=%d\n", roundtrip, forkshared, semok);
    return 0;
}
