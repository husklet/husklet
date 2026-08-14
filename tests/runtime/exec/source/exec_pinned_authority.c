#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define HL_EXEC_PIN_TEST_PRCTL 0x48504e54u
#define HL_EXEC_PIN_TEST_MAIN 1u
#define HL_EXEC_PIN_TEST_FINAL 2u

struct replacement {
    unsigned stage;
    const char *staged_link;
    const char *target;
};

static int has_argument(int argc, char **argv, const char *value) {
    for (int index = 1; index < argc; index++)
        if (strcmp(argv[index], value) == 0) return 1;
    return 0;
}

static int copy_file(const char *source, const char *target) {
    char bytes[65536];
    int input = open(source, O_RDONLY);
    int output = open(target, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    int ok = input >= 0 && output >= 0;
    for (;;) {
        ssize_t count = ok ? read(input, bytes, sizeof bytes) : -1;
        if (count == 0) break;
        if (count < 0) {
            ok = 0;
            break;
        }
        size_t offset = 0;
        while (offset < (size_t)count) {
            ssize_t written = write(output, bytes + offset, (size_t)count - offset);
            if (written <= 0) {
                ok = 0;
                break;
            }
            offset += (size_t)written;
        }
    }
    if (input >= 0) close(input);
    if (output >= 0 && close(output) != 0) ok = 0;
    return ok && chmod(target, 0755) == 0;
}

static void *replace_after_pin(void *opaque) {
    const struct replacement *replacement = opaque;
    for (int spin = 0; spin < 10000; spin++) {
        long phase = syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, 0, 0, 0, 0);
        if (phase == (long)replacement->stage) {
            if (rename(replacement->staged_link, replacement->target) != 0) _exit(91);
            if (syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, 3, 0, 0, 0) != 0) _exit(92);
            return NULL;
        }
        usleep(1000);
    }
    _exit(90);
}

static int run_case(const char *self, const char *root, unsigned stage) {
    char executable[256], script[256], bad[256], staged[256];
    snprintf(executable, sizeof executable, "%s/%s", root, stage == HL_EXEC_PIN_TEST_MAIN ? "program" : "interpreter");
    snprintf(script, sizeof script, "%s/script", root);
    snprintf(bad, sizeof bad, "%s/bad", root);
    snprintf(staged, sizeof staged, "%s/staged-link", root);
    if (!copy_file(self, executable)) return 0;
    int bad_fd = open(bad, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    if (bad_fd < 0 || write(bad_fd, "replacement must not execute\n", 29) != 29) return 0;
    close(bad_fd);
    if (symlink(bad, staged) != 0) return 0;
    if (stage == HL_EXEC_PIN_TEST_FINAL) {
        int script_fd = open(script, O_CREAT | O_TRUNC | O_WRONLY, 0755);
        char line[512];
        int length = snprintf(line, sizeof line, "#!%s\n", executable);
        if (script_fd < 0 || write(script_fd, line, (size_t)length) != length || close(script_fd) != 0) return 0;
    }
    if (syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, stage, 0, 0, 0) != 0) return 0;
    struct replacement replacement = {.stage = stage, .staged_link = staged, .target = executable};
    pthread_t helper;
    if (pthread_create(&helper, NULL, replace_after_pin, &replacement) != 0) return 0;
    char *arguments[] = {(char *)(stage == HL_EXEC_PIN_TEST_MAIN ? executable : script),
                         (char *)(stage == HL_EXEC_PIN_TEST_MAIN ? "main-child" : "script-child"), NULL};
    execv(arguments[0], arguments);
    return 0;
}

static int reap(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) ? WEXITSTATUS(status) : 255;
}

int main(int argc, char **argv) {
    if (has_argument(argc, argv, "main-child")) return 41;
    if (has_argument(argc, argv, "script-child")) return 42;
    char self[4096];
    ssize_t length = readlink("/proc/self/exe", self, sizeof self - 1);
    if (length <= 0)
        snprintf(self, sizeof self, "%s", argv[0]);
    else
        self[length] = 0;
    char root[128];
    snprintf(root, sizeof root, "/tmp/hl_exec_pin_%d", (int)getpid());
    if (mkdir(root, 0755) != 0) return 1;
    pid_t main_child = fork();
    if (main_child == 0) _exit(run_case(self, root, HL_EXEC_PIN_TEST_MAIN) ? 0 : 93);
    int main_result = main_child > 0 ? reap(main_child) : 255;
    pid_t script_child = fork();
    if (script_child == 0) _exit(run_case(self, root, HL_EXEC_PIN_TEST_FINAL) ? 0 : 93);
    int script_result = script_child > 0 ? reap(script_child) : 255;
    printf("main_pinned=%d final_shebang_pinned=%d\n", main_result == 41, script_result == 42);
    return main_result == 41 && script_result == 42 ? 0 : 1;
}
