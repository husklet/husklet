#include <signal.h>
#include <unistd.h>

static void stop(int signal_number) {
    (void)signal_number;
    _exit(42);
}

int main(void) {
    if (signal(SIGALRM, stop) == SIG_ERR) return 1;
    alarm(1);
    __asm__ volatile("movz x0,#0\n"
                     "1: add x0,x0,#1\n"
                     "cbnz x0,1b\n"
                     : : : "x0", "cc", "memory");
    return 2;
}
