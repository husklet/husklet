#include <stdio.h>
#include <sys/ptrace.h>

int main() {
    long r = ptrace(PTRACE_TRACEME, 0, 0, 0);
    printf("PTRACE=%ld\n", r);
    return 0;
}
