#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

struct state {
    pthread_mutex_t mutex;
    pthread_cond_t condition;
    uint32_t generation, ack, child_ready;
};

static void pause_briefly(void) {
    struct timespec t = {0, 1000000};
    nanosleep(&t, NULL);
}

static int wait_file(const char *path) {
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (access(path, F_OK) == 0) return 0;
        pause_briefly();
    }
    return -1;
}

static struct timespec deadline(void) {
    struct timespec result;
    clock_gettime(CLOCK_REALTIME, &result);
    result.tv_sec += 30;
    return result;
}

static int wait_sleeping(pid_t process) {
    char path[64], line[512];
    snprintf(path, sizeof path, "/proc/%d/stat", (int)process);
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        int descriptor = open(path, O_RDONLY);
        ssize_t count = descriptor < 0 ? -1 : read(descriptor, line, sizeof line - 1);
        if (descriptor >= 0) close(descriptor);
        if (count > 0) {
            line[count] = 0;
            char *end = strrchr(line, ')');
            if (end != NULL && end[1] == ' ' && end[2] == 'S') return 0;
        }
        pause_briefly();
    }
    return -1;
}

static int reap_bounded(pid_t child) {
    int status = 0;
    for (unsigned attempt = 0; attempt < 30000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
        pause_briefly();
    }
    kill(child, SIGKILL);
    for (unsigned attempt = 0; attempt < 5000; ++attempt) {
        if (waitpid(child, &status, WNOHANG) == child) break;
        pause_briefly();
    }
    return -1;
}

static int fail_child(pid_t child, int result) {
    kill(child, SIGKILL);
    (void)reap_bounded(child);
    return result;
}

int main(int argc, char **argv) {
    if (argc != 2) return 10;
    char output[1024], cycle1[1024], cycle2[1024], finish[1024];
    snprintf(output, sizeof output, "%s/output", argv[1]);
    snprintf(cycle1, sizeof cycle1, "%s/cycle1", argv[1]);
    snprintf(cycle2, sizeof cycle2, "%s/cycle2", argv[1]);
    snprintf(finish, sizeof finish, "%s/finish", argv[1]);
    int log = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    int fd = (int)syscall(SYS_memfd_create, "checkpoint-pshared-cond", 0u);
    long page = sysconf(_SC_PAGESIZE);
    if (log < 0 || fd < 0 || page <= 0 || ftruncate(fd, page) != 0) return 11;
    struct state *shared = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (shared == MAP_FAILED) return 12;
    pthread_mutexattr_t mutex_attr;
    pthread_condattr_t cond_attr;
    if (pthread_mutexattr_init(&mutex_attr) != 0 ||
        pthread_mutexattr_setpshared(&mutex_attr, PTHREAD_PROCESS_SHARED) != 0 ||
        pthread_condattr_init(&cond_attr) != 0 ||
        pthread_condattr_setpshared(&cond_attr, PTHREAD_PROCESS_SHARED) != 0 ||
        pthread_mutex_init(&shared->mutex, &mutex_attr) != 0 || pthread_cond_init(&shared->condition, &cond_attr) != 0)
        return 13;
    dprintf(log, "BOOT\n");
    pid_t child = fork();
    if (child < 0) return 14;
    if (child == 0) {
        if (pthread_mutex_lock(&shared->mutex) != 0) _exit(20);
        shared->child_ready = 1;
        pthread_cond_broadcast(&shared->condition);
        for (uint32_t generation = 1; generation <= 2; ++generation) {
            struct timespec timeout = deadline();
            while (shared->generation < generation)
                if (pthread_cond_timedwait(&shared->condition, &shared->mutex, &timeout) != 0) _exit(21);
            shared->ack = generation;
            pthread_cond_broadcast(&shared->condition);
        }
        pthread_mutex_unlock(&shared->mutex);
        if (wait_file(finish) != 0) _exit(22);
        _exit(0);
    }
    if (pthread_mutex_lock(&shared->mutex) != 0) return fail_child(child, 15);
    struct timespec ready_timeout = deadline();
    while (!shared->child_ready)
        if (pthread_cond_timedwait(&shared->condition, &shared->mutex, &ready_timeout) != 0) {
            pthread_mutex_unlock(&shared->mutex);
            return fail_child(child, 16);
        }
    pthread_mutex_unlock(&shared->mutex);
    if (wait_sleeping(child) != 0) return fail_child(child, 17);
    dprintf(log, "READY\n");
    const char *cycles[2] = {cycle1, cycle2};
    for (uint32_t generation = 1; generation <= 2; ++generation) {
        if (wait_file(cycles[generation - 1]) != 0) return fail_child(child, 18);
        if (pthread_mutex_lock(&shared->mutex) != 0) return fail_child(child, 19);
        shared->generation = generation;
        pthread_cond_broadcast(&shared->condition);
        struct timespec ack_timeout = deadline();
        while (shared->ack < generation)
            if (pthread_cond_timedwait(&shared->condition, &shared->mutex, &ack_timeout) != 0) {
                pthread_mutex_unlock(&shared->mutex);
                return fail_child(child, 20);
            }
        pthread_mutex_unlock(&shared->mutex);
        if (generation == 1 && wait_sleeping(child) != 0) return fail_child(child, 21);
        dprintf(log, generation == 1 ? "CYCLE 1\n" : "DONE pshared-cond-ok\n");
    }
    if (wait_file(finish) != 0) return fail_child(child, 22);
    int status = 0;
    (void)status;
    return reap_bounded(child) == 0 ? 0 : 23;
}
