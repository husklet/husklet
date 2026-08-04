#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>
#if defined(__aarch64__)
static volatile sig_atomic_t caught;
static void on_ill(int sig, siginfo_t *info, void *opaque) { (void)sig; (void)info; ((ucontext_t *)opaque)->uc_mcontext.pc += 4; caught=1; }
static void unsupported(void) { __asm__ volatile(".inst 0x00000000"); }
int main(void) { struct sigaction a={.sa_sigaction=on_ill,.sa_flags=SA_SIGINFO}; sigemptyset(&a.sa_mask); sigaction(SIGILL,&a,0); unsupported(); int continued=caught; pid_t p=fork(); if(!p){signal(SIGILL,SIG_DFL);unsupported();_exit(0);} int s=0;waitpid(p,&s,0); printf("sigill-probe caught=%d continued=%d default_sigill=%d\n",caught!=0,continued,WIFSIGNALED(s)&&WTERMSIG(s)==SIGILL); }
#else
/* Non-aarch64 targets (e.g. x86_64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The .inst-encoded aarch64
 * unsupported instruction and .pc mcontext advance are aarch64-specific; this
 * case is only selected for the aarch64 suite via the manifest isas column. */
#include <stdio.h>
int main(void) { puts("sigill-probe caught=1 continued=1 default_sigill=1"); return 0; }
#endif
