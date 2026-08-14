#define _GNU_SOURCE
#include <errno.h>
#include <elf.h>
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
#define HL_EXEC_PIN_TEST_SHEBANG_HOP 4u
#define HL_EXEC_PIN_TEST_ENV_POISON 5u

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

static int run_multihop_case(const char *self, const char *root) {
    char intermediate[256], script[256], bad[256], staged[256];
    snprintf(intermediate, sizeof intermediate, "%s/intermediate", root);
    snprintf(script, sizeof script, "%s/multihop", root);
    snprintf(bad, sizeof bad, "%s/hop-bad", root);
    snprintf(staged, sizeof staged, "%s/hop-staged", root);
    int intermediate_fd = open(intermediate, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    char line[512];
    int length = snprintf(line, sizeof line, "#!%s\n", self);
    if (intermediate_fd < 0 || write(intermediate_fd, line, (size_t)length) != length || close(intermediate_fd) != 0)
        return 0;
    int script_fd = open(script, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    length = snprintf(line, sizeof line, "#!%s\n", intermediate);
    if (script_fd < 0 || write(script_fd, line, (size_t)length) != length || close(script_fd) != 0) return 0;
    int bad_fd = open(bad, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    if (bad_fd < 0 || write(bad_fd, "replacement must not parse\n", 27) != 27 || close(bad_fd) != 0) return 0;
    if (symlink(bad, staged) != 0 ||
        syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, HL_EXEC_PIN_TEST_SHEBANG_HOP, 0, 0, 0) != 0)
        return 0;
    struct replacement replacement = {
        .stage = HL_EXEC_PIN_TEST_SHEBANG_HOP, .staged_link = staged, .target = intermediate};
    pthread_t helper;
    if (pthread_create(&helper, NULL, replace_after_pin, &replacement) != 0) return 0;
    char *arguments[] = {script, (char *)"multihop-child", NULL};
    execv(script, arguments);
    return 0;
}

static int prepare_dynamic_payload(const char *source, const char *payload, const char *interpreter,
                                   char *original_interpreter, size_t original_size) {
    if (!copy_file(source, payload)) return 0;
    int descriptor = open(payload, O_RDWR);
    Elf64_Ehdr header;
    if (descriptor < 0 || pread(descriptor, &header, sizeof header, 0) != sizeof header ||
        memcmp(header.e_ident, ELFMAG, SELFMAG) != 0 || header.e_ident[EI_CLASS] != ELFCLASS64 ||
        header.e_phentsize != sizeof(Elf64_Phdr))
        return 0;
    for (Elf64_Half index = 0; index < header.e_phnum; index++) {
        Elf64_Phdr program;
        off_t offset = (off_t)header.e_phoff + (off_t)index * (off_t)sizeof program;
        if (pread(descriptor, &program, sizeof program, offset) != sizeof program) return 0;
        if (program.p_type != PT_INTERP || program.p_filesz == 0 || program.p_filesz > original_size) continue;
        if (pread(descriptor, original_interpreter, (size_t)program.p_filesz, (off_t)program.p_offset) !=
            (ssize_t)program.p_filesz)
            return 0;
        original_interpreter[original_size - 1] = 0;
        size_t replacement_size = strlen(interpreter) + 1;
        if (replacement_size > program.p_filesz) return 0;
        char replacement[256] = {0};
        memcpy(replacement, interpreter, replacement_size);
        int ok = pwrite(descriptor, replacement, replacement_size, (off_t)program.p_offset) ==
                 (ssize_t)replacement_size;
        program.p_filesz = replacement_size;
        ok = ok && pwrite(descriptor, &program, sizeof program, offset) == sizeof program;
        close(descriptor);
        return ok;
    }
    close(descriptor);
    return 0;
}

static int run_pt_interp_case(const char *root) {
    char payload[256], interpreter[256], bad[256], staged[256], original[256];
    snprintf(payload, sizeof payload, "%s/dynamic", root);
    snprintf(interpreter, sizeof interpreter, "%s/i", root);
    snprintf(bad, sizeof bad, "%s/pt-bad", root);
    snprintf(staged, sizeof staged, "%s/pt-staged", root);
    if (!prepare_dynamic_payload("/bin/true", payload, interpreter, original, sizeof original) ||
        !copy_file(original, interpreter))
        return 0;
    int bad_fd = open(bad, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    if (bad_fd < 0 || write(bad_fd, "replacement must not load\n", 26) != 26 || close(bad_fd) != 0) return 0;
    if (symlink(bad, staged) != 0 || syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, HL_EXEC_PIN_TEST_FINAL, 0, 0, 0) != 0)
        return 0;
    struct replacement replacement = {.stage = HL_EXEC_PIN_TEST_FINAL, .staged_link = staged, .target = interpreter};
    pthread_t helper;
    if (pthread_create(&helper, NULL, replace_after_pin, &replacement) != 0) return 0;
    char *arguments[] = {(char *)"true", NULL};
    execv(payload, arguments);
    return 0;
}

static int reap(pid_t child) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status) ? WEXITSTATUS(status) : 255;
}

static int failed_exec_keeps_environment(const char *root) {
    char malformed[256];
    snprintf(malformed, sizeof malformed, "%s/failed-env", root);
    int descriptor = open(malformed, O_CREAT | O_TRUNC | O_WRONLY, 0755);
    int written = descriptor >= 0 && write(descriptor, "#!\n", 3) == 3;
    if (descriptor >= 0) close(descriptor);
    if (!written) return 0;
    char *arguments[] = {malformed, NULL};
    char *environment[] = {(char *)"HL_FAILED_EXEC_POISON=1", NULL};
    execve(malformed, arguments, environment);
    return errno == ENOEXEC && syscall(SYS_prctl, HL_EXEC_PIN_TEST_PRCTL, HL_EXEC_PIN_TEST_ENV_POISON, 0, 0, 0) == 0;
}

int main(int argc, char **argv) {
    if (has_argument(argc, argv, "main-child")) return 41;
    if (has_argument(argc, argv, "script-child")) return 42;
    if (has_argument(argc, argv, "multihop-child")) return 43;
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
    pid_t multihop_child = fork();
    if (multihop_child == 0) _exit(run_multihop_case(self, root) ? 0 : 93);
    int multihop_result = multihop_child > 0 ? reap(multihop_child) : 255;
    char pt_root[64];
    snprintf(pt_root, sizeof pt_root, "/tmp/hp%d", (int)getpid());
    mkdir(pt_root, 0755);
    pid_t pt_child = fork();
    if (pt_child == 0) _exit(run_pt_interp_case(pt_root) ? 0 : 93);
    int pt_result = pt_child > 0 ? reap(pt_child) : 255;
    int environment_atomic = failed_exec_keeps_environment(root);
    printf("main_pinned=%d final_shebang_pinned=%d shebang_hop_pinned=%d pt_interp_pinned=%d failed_env_atomic=%d\n",
           main_result == 41, script_result == 42, multihop_result == 43, pt_result == 0, environment_atomic);
    return main_result == 41 && script_result == 42 && multihop_result == 43 && pt_result == 0 && environment_atomic
               ? 0
               : 1;
}
