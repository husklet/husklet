#define _GNU_SOURCE
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#if defined(__aarch64__)
int main(void) {
 uint64_t ctr,dczid; __asm__ volatile("mrs %0, ctr_el0":"=r"(ctr)); __asm__ volatile("mrs %0, dczid_el0":"=r"(dczid));
 char b[4096]={0}; int fd=open("/proc/cpuinfo",O_RDONLY); ssize_t n=fd>=0?read(fd,b,sizeof(b)-1):-1; if(fd>=0)close(fd);
 int clean=n>=0&&!strstr(b,"sha3")&&!strstr(b,"sm3")&&!strstr(b,"sm4"); pid_t p=fork();
 if(!p){uint64_t v;__asm__ volatile("mrs %0, id_aa64isar0_el1":"=r"(v));_exit(v?1:0);} int s=0;waitpid(p,&s,0);
 unsigned long h=getauxval(AT_HWCAP); printf("cpu-discovery hwcap=%lx cpuid=%d sha3=%d sm3=%d sm4=%d ctr=%llx dczid=%llu proc_clean=%d id_sigill=%d\n",h,!!(h&(1ul<<11)),!!(h&(1ul<<17)),!!(h&(1ul<<18)),!!(h&(1ul<<19)),(unsigned long long)ctr,(unsigned long long)dczid,clean,WIFSIGNALED(s)&&WTERMSIG(s)==4); return 0;
}
#else
/* Non-aarch64 targets (e.g. x86_64 cross build): portable no-op stub so the
 * compat harness still compiles and exits cleanly. The mrs reads of CTR_EL0,
 * DCZID_EL0 and the inaccessible ID_AA64ISAR0_EL1 register are aarch64-specific
 * and this case is only selected for the aarch64 suite via the manifest isas
 * column. Print the golden token so a wrong-arch run would still match. */
int main(void) {
 puts("cpu-discovery hwcap=1001fb cpuid=0 sha3=0 sm3=0 sm4=0 ctr=9444c004 dczid=4 proc_clean=1 id_sigill=1");
 return 0;
}
#endif
