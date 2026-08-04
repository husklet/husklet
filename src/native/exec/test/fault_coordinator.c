#define _GNU_SOURCE
#include "../include/executor.h"

#include <signal.h>
#include <stdlib.h>
#include <stdint.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(value) do { if (!(value)) abort(); } while (0)

static volatile sig_atomic_t chained;
static volatile sig_atomic_t repaired;

int hl_native_fault_thread_prepare(uint64_t program, uint64_t address, void *context) {
    (void)program;
    (void)address;
    (void)context;
    return 0;
}

hl_native_status hl_native_fault_thread_after_fork_child(void) {
    repaired++;
    return HL_NATIVE_OK;
}

#include "../src/fault/coordinator.c"

static void prior_handler(int signal) {
    (void)signal;
    chained++;
}

static void set_handler(int signal, void (*handler)(int), struct sigaction *prior) {
    struct sigaction action = {0};
    action.sa_handler = handler;
    sigemptyset(&action.sa_mask);
    CHECK(sigaction(signal, &action, prior) == 0);
}

int main(void) {
    struct sigaction original_segv;
    struct sigaction original_bus;
    set_handler(SIGSEGV, prior_handler, &original_segv);
    set_handler(SIGBUS, SIG_IGN, &original_bus);

    CHECK(hl_native_fault_process_acquire() == HL_NATIVE_OK);
    CHECK(hl_native_fault_process_acquire() == HL_NATIVE_OK);
    CHECK(raise(SIGSEGV) == 0 && chained == 1);
    CHECK(raise(SIGBUS) == 0);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);
    CHECK(raise(SIGSEGV) == 0 && chained == 2);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);
    CHECK(raise(SIGSEGV) == 0 && chained == 3);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_STATE);

    CHECK(hl_native_fault_process_acquire() == HL_NATIVE_OK);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        CHECK(repaired == 1);
        CHECK(raise(SIGSEGV) == 0 && chained == 4);
        CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);
        _exit(0);
    }
    int status = 0;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);

    struct sigaction default_bus = {0};
    default_bus.sa_handler = SIG_DFL;
    sigemptyset(&default_bus.sa_mask);
    CHECK(sigaction(SIGBUS, &default_bus, NULL) == 0);
    CHECK(hl_native_fault_process_acquire() == HL_NATIVE_OK);
    child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        raise(SIGBUS);
        _exit(99);
    }
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGBUS);
    CHECK(hl_native_fault_process_release() == HL_NATIVE_OK);

    CHECK(sigaction(SIGSEGV, &original_segv, NULL) == 0);
    CHECK(sigaction(SIGBUS, &original_bus, NULL) == 0);
    return 0;
}
