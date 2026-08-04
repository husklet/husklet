// ucontext round-trips: getcontext/setcontext resume, makecontext(fn,args) runs on its own stack with
// uc_link chaining, and swapcontext ping-pong preserves locals + a counter + callee-saved registers
// across N switches (boost.context-style coroutines depend on exact callee-saved survival).
#define _GNU_SOURCE
#include <stdio.h>
#include <ucontext.h>

static ucontext_t uc_main, uc_co;
static volatile int flag;

static void setctx_probe(void) {
    // getcontext -> flip a flag -> setcontext returns here once (flag guards the re-entry).
    flag = 0;
    getcontext(&uc_co);
    flag++;
    if (flag == 1) setcontext(&uc_co); // resumes at getcontext; flag now 2 on the retry
}

static volatile long co_counter;
static int co_arg_seen;

static void coroutine(int a, int b, int c) {
    co_arg_seen = (a == 11 && b == 22 && c == 33);
    for (int i = 0; i < 5; i++) {
        register long cs asm(
#if defined(__x86_64__)
            "r15"
#else
            "x24"
#endif
        ) = 0xc0ffee + i;
        co_counter++;
        swapcontext(&uc_co, &uc_main); // yield to main
        // after resume, the callee-saved reg must still hold our value
        if ((long)cs != 0xc0ffee + i) co_counter += 100000; // corruption marker
        asm volatile("" : : "r"(cs));
    }
    // falls off the end -> uc_link (uc_main) resumes
}

int main(void) {
    setctx_probe();
    int setctx_ok = flag == 2;

    static char stack[64 * 1024];
    getcontext(&uc_co);
    uc_co.uc_stack.ss_sp = stack;
    uc_co.uc_stack.ss_size = sizeof stack;
    uc_co.uc_link = &uc_main;
    makecontext(&uc_co, (void (*)(void))coroutine, 3, 11, 22, 33);

    long ping = 0;
    for (int i = 0; i < 5; i++) {
        swapcontext(&uc_main, &uc_co); // enter/resume coroutine
        ping++;                        // a main-side local preserved across each switch
    }
    swapcontext(&uc_main, &uc_co); // final resume; coroutine returns via uc_link

    printf("ucontext setctx=%d args=%d counter=%d ping=%d\n",
           setctx_ok, co_arg_seen, co_counter == 5, ping == 5);
    return 0;
}
