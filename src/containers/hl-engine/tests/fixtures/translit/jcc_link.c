#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>

extern long linked_target(long, long, long, long);
extern long linked_source(long, long, long, long);
extern long cold_source(long, long, long, long);

// Two public entry points on one page. main enters linked_target first, making its descriptor and emitted
// body live before linked_source is translated. linked_source's taken JCC can therefore embed one
// immutable rel32 edge without backpatching. RCX is the fourth ABI argument and remains live across the
// link; both answers make a stale/spilled RCX observable.
__asm__(".pushsection .text.jcc_link,\"ax\",@progbits\n"
        ".balign 4096\n"
        ".global linked_target\n.type linked_target,@function\n"
        "linked_target:\n lea 7(%rsi,%rcx),%rax\n ret\n"
        ".size linked_target,.-linked_target\n"
        ".balign 16\n"
        ".global linked_source\n.type linked_source,@function\n"
        "linked_source:\n test %rdi,%rdi\n jnz linked_target\n lea 3(%rsi,%rcx),%rax\n ret\n"
        ".size linked_source,.-linked_source\n"
        ".balign 16\n"
        ".type cold_target,@function\n"
        "cold_target:\n lea 11(%rsi,%rcx),%rax\n ret\n"
        ".size cold_target,.-cold_target\n"
        ".balign 16\n"
        ".global cold_source\n.type cold_source,@function\n"
        "cold_source:\n test %rdi,%rdi\n jnz cold_target\n lea 5(%rsi,%rcx),%rax\n ret\n"
        ".size cold_source,.-cold_source\n"
        ".popsection\n");

static volatile sig_atomic_t caught;

static void alarm_handler(int signal) {
    (void)signal;
    caught++;
}

int main(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_handler = alarm_handler;
    sigaction(SIGALRM, &action, NULL);

    long warm = linked_target(0, 31, 0, 4);
    // cold_target has not been entered, so this source remains an ordinary immutable dispatcher exit.
    long cold = cold_source(1, 31, 0, 4);
    long taken = 0, fall = 0;
    struct itimerval timer = {{0, 100}, {0, 100}};
    setitimer(ITIMER_REAL, &timer, NULL);
    for (int i = 0; i < 200000; i++) {
        taken += linked_source(1, i & 31, 0, 4);
        fall += linked_source(0, i & 31, 0, 4);
    }
    memset(&timer, 0, sizeof timer);
    setitimer(ITIMER_REAL, &timer, NULL);
    printf("warm=%ld cold=%ld taken=%ld fall=%ld signals=%d\n", warm, cold, taken, fall, caught != 0);
    return warm != 42 || cold != 46 || caught == 0;
}
