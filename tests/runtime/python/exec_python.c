#include <errno.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <elf.h>
#include <unistd.h>

int main(int argc, char **argv) {
    static const char interpreter[] = "/usr/local/bin/python3";
    static const char proof[] = "/tmp/husklet-cpython-proof";
    char **arguments = calloc((size_t)argc + 1, sizeof(*arguments));
    if (arguments == NULL) {
        fputs("exec-python: allocation failed\n", stderr);
        return 125;
    }
    arguments[0] = (char *)interpreter;
    for (int index = 1; index < argc; ++index) {
        arguments[index] = argv[index];
    }
    const pid_t child = fork();
    if (child == 0) {
        FILE *diagnostic = freopen("/tmp/husklet-cpython-stderr", "w", stderr);
        if (diagnostic == NULL) {
            _exit(126);
        }
        if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0) {
            perror("exec-python: ptrace");
            _exit(126);
        }
        execv(interpreter, arguments);
        const int error = errno;
        fprintf(stderr, "exec-python: %s: ", interpreter);
        errno = error;
        perror(NULL);
        _exit(127);
    }
    if (child < 0) {
        perror("exec-python: fork");
        free(arguments);
        return 126;
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child) {
        perror("exec-python: waitpid");
        free(arguments);
        return 126;
    }
    if (WIFSTOPPED(status) && ptrace(PTRACE_CONT, child, NULL, NULL) != 0) {
        perror("exec-python: continue");
        return 126;
    }
    while (WIFSTOPPED(status)) {
        if (waitpid(child, &status, 0) != child) {
            perror("exec-python: trace wait");
            return 126;
        }
        if (!WIFSTOPPED(status)) {
            break;
        }
        const int signal = WSTOPSIG(status);
        if (signal == SIGSEGV) {
            unsigned long registers[64] = {0};
            struct iovec view = {.iov_base = registers, .iov_len = sizeof(registers)};
            if (ptrace(PTRACE_GETREGSET, child, (void *)(uintptr_t)NT_PRSTATUS, &view) == 0) {
#if defined(__aarch64__)
                fprintf(stderr, "exec-python: fault pc=%#lx sp=%#lx\n", registers[32], registers[31]);
#elif defined(__x86_64__)
                fprintf(stderr, "exec-python: fault pc=%#lx sp=%#lx\n", registers[16], registers[19]);
#endif
            }
        }
        if (ptrace(PTRACE_CONT, child, NULL, (void *)(uintptr_t)signal) != 0) {
            perror("exec-python: signal continue");
            return 126;
        }
    }
    free(arguments);
    if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
        FILE *input = fopen(proof, "rb");
        if (input == NULL) {
            perror("exec-python: proof");
            return 125;
        }
        char buffer[4096];
        size_t length = 0;
        while ((length = fread(buffer, 1, sizeof(buffer), input)) != 0) {
            if (fwrite(buffer, 1, length, stdout) != length) {
                fclose(input);
                return 125;
            }
        }
        if (ferror(input) != 0 || fclose(input) != 0) {
            return 125;
        }
        return 0;
    }
    FILE *diagnostic = fopen("/tmp/husklet-cpython-stderr", "rb");
    if (diagnostic != NULL) {
        char buffer[4096];
        size_t length = 0;
        while ((length = fread(buffer, 1, sizeof(buffer), diagnostic)) != 0) {
            (void)fwrite(buffer, 1, length, stderr);
        }
        (void)fclose(diagnostic);
    }
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    return 128 + WTERMSIG(status);
}
