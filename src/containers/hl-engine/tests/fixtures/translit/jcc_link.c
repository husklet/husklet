#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/time.h>

extern long linked_target(long, long, long, long);
extern long linked_source(long, long, long, long);
extern long cold_source(long, long, long, long);
extern long same_final(long, long, long, long);
extern long same_terminal_target(long, long, long, long);
extern long same_terminal_source(long, long, long, long);
extern long call_terminal_target(long, long, long, long);
extern long different_terminal_source(long, long, long, long);

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
        // Warm same_final before publishing same_terminal_source, making the source's residual JMP
        // eligible. The linked target's own residual JMP was published while same_cold_final was cold,
        // so same-family ingress must retain the target terminal's TARGET_UNMAPPED disposition.
        ".balign 16\n"
        ".global same_final\n.type same_final,@function\n"
        "same_final:\n lea 13(%rsi,%rcx),%rax\n ret\n"
        ".size same_final,.-same_final\n"
        ".balign 16\n"
        ".type same_cold_final,@function\n"
        "same_cold_final:\n lea 17(%rsi,%rcx),%rax\n ret\n"
        ".size same_cold_final,.-same_cold_final\n"
        ".balign 16\n"
        ".global same_terminal_target\n.type same_terminal_target,@function\n"
        "same_terminal_target:\n jmp same_cold_final\n"
        ".size same_terminal_target,.-same_terminal_target\n"
        ".balign 16\n"
        ".global same_terminal_source\n.type same_terminal_source,@function\n"
        "same_terminal_source:\n test %rdi,%rdi\n jnz same_terminal_target\n jmp same_final\n"
        ".size same_terminal_source,.-same_terminal_source\n"
        // This linked target executes a direct CALL while its dispatcher-entry source owns a direct JMP.
        // The differing-family case must count the target CALL rather than dropping it.
        ".balign 16\n"
        ".global call_terminal_target\n.type call_terminal_target,@function\n"
        "call_terminal_target:\n call call_leaf\n ret\n"
        ".size call_terminal_target,.-call_terminal_target\n"
        ".balign 16\n"
        ".type call_leaf,@function\n"
        "call_leaf:\n lea 19(%rsi,%rcx),%rax\n ret\n"
        ".size call_leaf,.-call_leaf\n"
        ".balign 16\n"
        ".global different_terminal_source\n.type different_terminal_source,@function\n"
        "different_terminal_source:\n test %rdi,%rdi\n jnz call_terminal_target\n jmp same_final\n"
        ".size different_terminal_source,.-different_terminal_source\n"
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
    long same_final_warm = same_final(0, 31, 0, 4);
    long same_target_warm = same_terminal_target(0, 31, 0, 4);
    long call_target_warm = call_terminal_target(0, 31, 0, 4);
    long same_terminal = 0, different_terminal = 0;
    long expected_same = 0, expected_different = 0;
    for (int i = 0; i < 1000; i++) {
        same_terminal += same_terminal_source(1, i & 31, 0, 4);
        different_terminal += different_terminal_source(1, i & 31, 0, 4);
        expected_same += (i & 31) + 21;
        expected_different += (i & 31) + 23;
    }
    long taken = 0, fall = 0;
    struct itimerval timer = {{0, 100}, {0, 100}};
    setitimer(ITIMER_REAL, &timer, NULL);
    for (int i = 0; i < 200000; i++) {
        taken += linked_source(1, i & 31, 0, 4);
        fall += linked_source(0, i & 31, 0, 4);
    }
    memset(&timer, 0, sizeof timer);
    setitimer(ITIMER_REAL, &timer, NULL);
    printf("warm=%ld cold=%ld same=%ld different=%ld taken=%ld fall=%ld signals=%d\n", warm, cold,
           same_terminal, different_terminal, taken, fall, caught != 0);
    return warm != 42 || cold != 46 || same_final_warm != 48 || same_target_warm != 52 ||
           call_target_warm != 54 || same_terminal != expected_same || different_terminal != expected_different ||
           caught == 0;
}
