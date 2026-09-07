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
                     "movz x1,#1\n"
                     "1: madd x0,x0,x1,x1\n"
                     "cbnz x0,1b\n"
                     : : : "x0", "x1", "cc", "memory");
    return 2;
}
