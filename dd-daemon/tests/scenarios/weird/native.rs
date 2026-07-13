//! Native low-level corners (all compiled+run via the `cc()` helper, so all xfail on Linux behind the
//! toolchain gap; each also probes a deeper corner): self-modifying / dynamic code (RWX exec, SMC
//! re-translation), signals / faults / threads (guest signal delivery, clone/futex/atomics), and
//! exotic syscalls (memfd/eventfd/timerfd/inotify/prctl/getrandom/ptrace/affinity/uffd/io_uring/bpf/seccomp).

use crate::scenario::Scenario;
use super::cc;

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ====================== self-modifying / dynamic code (native) =========================
        // mmap(RWX), write machine code, jump to it — the canonical SMC / DBT translate-on-execute test.
        cc("weird/self-modifying-rwx", "-O0", r#"#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
int main(void){
  unsigned char *m=mmap(0,4096,PROT_READ|PROT_WRITE|PROT_EXEC,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
  if(m==MAP_FAILED){perror("mmap");return 1;}
#if defined(__x86_64__)
  unsigned char code[]={0xB8,0x2A,0x00,0x00,0x00,0xC3};
#elif defined(__aarch64__)
  unsigned char code[]={0x40,0x05,0x80,0x52,0xC0,0x03,0x5F,0xD6};
#endif
  memcpy(m,code,sizeof(code));
  __builtin___clear_cache((char*)m,(char*)m+sizeof(code));
  int (*f)(void)=(int(*)(void))m;
  printf("RWX=%d\n",f());
  return 0;
}"#).has("RWX=42"),
        // OVERWRITE an already-executed RWX page with new code and re-run — forces the JIT to invalidate
        // its translated-block cache for that guest page (the killer DBT corner).
        cc("weird/smc-rewrite", "-O0", r#"#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
typedef int(*fn)(void);
int main(void){
  unsigned char *m=mmap(0,4096,PROT_READ|PROT_WRITE|PROT_EXEC,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
  if(m==MAP_FAILED){perror("mmap");return 1;}
#if defined(__x86_64__)
  unsigned char c1[]={0xB8,0x01,0x00,0x00,0x00,0xC3};
  unsigned char c2[]={0xB8,0x02,0x00,0x00,0x00,0xC3};
#elif defined(__aarch64__)
  unsigned char c1[]={0x20,0x00,0x80,0x52,0xC0,0x03,0x5F,0xD6};
  unsigned char c2[]={0x40,0x00,0x80,0x52,0xC0,0x03,0x5F,0xD6};
#endif
  memcpy(m,c1,sizeof c1); __builtin___clear_cache((char*)m,(char*)m+16);
  int r1=((fn)m)();
  memcpy(m,c2,sizeof c2); __builtin___clear_cache((char*)m,(char*)m+16);
  int r2=((fn)m)();
  printf("SMC=%d,%d\n",r1,r2); return 0;
}"#).has("SMC=1,2"),

        // ============================ signals / faults / threads (native) ======================
        // Install a SIGSEGV handler, fault on a null write, recover via siglongjmp — the JIT must
        // synthesize + deliver a guest signal from a fault in translated code, then resume.
        cc("weird/sigsegv-recover", "-O0", r#"#include <stdio.h>
#include <signal.h>
#include <setjmp.h>
static sigjmp_buf jb;
static void h(int s){ (void)s; siglongjmp(jb,1); }
int main(){
  signal(SIGSEGV,h);
  if(sigsetjmp(jb,1)==0){ volatile int *p=0; *p=1; printf("NOFAULT\n"); }
  else printf("RECOVERED\n");
  return 0;
}"#).has("RECOVERED"),
        // 8 pthreads × 100k atomic increments — TLS, clone/futex, and atomics under the translator.
        cc("weird/pthread-atomics", "-O2 -pthread", r#"#include <stdio.h>
#include <pthread.h>
#include <stdatomic.h>
atomic_long c=0;
void*w(void*x){ (void)x; for(int i=0;i<100000;i++) atomic_fetch_add(&c,1); return 0; }
int main(){ pthread_t t[8]; for(int i=0;i<8;i++)pthread_create(&t[i],0,w,0);
  for(int i=0;i<8;i++)pthread_join(t[i],0);
  printf("THREADS=%ld\n",(long)c); return 0;}"#).has("THREADS=800000"),

        // ================================ exotic syscalls (native) =============================
        // memfd_create — anonymous in-memory fd.
        cc("weird/memfd-create", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
int main(){
  int fd=memfd_create("dd",0);
  if(fd<0){perror("memfd");return 1;}
  write(fd,"dd-memfd-ok",11);
  char b[32]={0}; lseek(fd,0,SEEK_SET); read(fd,b,11);
  printf("%s\n",b); return 0;
}"#).has("dd-memfd-ok"),
        // eventfd counter semantics (41 + 1 → read 42, reset).
        cc("weird/eventfd", "-O2", r#"#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
int main(){
  int fd=eventfd(0,0);
  uint64_t v=41; write(fd,&v,8); v=1; write(fd,&v,8);
  uint64_t r; read(fd,&r,8);
  printf("EVENTFD=%llu\n",(unsigned long long)r); return 0;
}"#).has("EVENTFD=42"),
        // timerfd: one 50ms one-shot expiration → read returns 1.
        cc("weird/timerfd", "-O2", r#"#include <stdio.h>
#include <sys/timerfd.h>
#include <unistd.h>
#include <stdint.h>
int main(){
  int fd=timerfd_create(CLOCK_MONOTONIC,0);
  if(fd<0){perror("timerfd");return 1;}
  struct itimerspec its={{0,0},{0,50000000}};
  timerfd_settime(fd,0,&its,0);
  uint64_t e; read(fd,&e,8);
  printf("TIMERFD=%llu\n",(unsigned long long)e); return 0;
}"#).has("TIMERFD=1"),
        // inotify: watch /tmp, create a file, read the IN_CREATE event.
        cc("weird/inotify", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <sys/inotify.h>
#include <unistd.h>
#include <fcntl.h>
int main(){
  int fd=inotify_init1(0);
  if(fd<0){perror("inotify");return 1;}
  inotify_add_watch(fd,"/tmp",IN_CREATE);
  int f=open("/tmp/dd-inotify",O_CREAT|O_WRONLY,0644); close(f);
  char buf[4096]; read(fd,buf,sizeof buf);
  struct inotify_event *ev=(void*)buf;
  printf("INOTIFY=%s\n", ev->len?ev->name:"none"); return 0;
}"#).has("INOTIFY=dd-inotify"),
        // prctl PR_SET_NAME / PR_GET_NAME round-trip.
        cc("weird/prctl-name", "-O2", r#"#include <stdio.h>
#include <sys/prctl.h>
int main(){
  prctl(PR_SET_NAME,"dd-proc");
  char n[16]={0}; prctl(PR_GET_NAME,n);
  printf("PRCTL=%s\n",n); return 0;
}"#).has("PRCTL=dd-proc"),
        // getrandom(2): 16 bytes from the kernel CSPRNG.
        cc("weird/getrandom", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <sys/random.h>
int main(){char b[16]; ssize_t n=getrandom(b,16,0); printf("GETRANDOM=%zd\n",n); return 0;}"#)
            .has("GETRANDOM=16"),
        // ptrace(PTRACE_TRACEME): the anti-debug / tracer-attach primitive.
        cc("weird/ptrace-traceme", "-O2", r#"#include <stdio.h>
#include <sys/ptrace.h>
int main(){
  long r=ptrace(PTRACE_TRACEME,0,0,0);
  printf("PTRACE=%ld\n", r); return 0;
}"#).has("PTRACE=0"),
        // sched_getaffinity: CPU-mask population (same family as the mongo cpu-topology gap).
        cc("weird/sched-affinity", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <sched.h>
int main(){
  cpu_set_t s; CPU_ZERO(&s);
  if(sched_getaffinity(0,sizeof s,&s)<0){perror("aff");return 1;}
  printf("AFFINITY=%s\n", CPU_COUNT(&s)>0?"nonzero":"zero"); return 0;
}"#).has("AFFINITY=nonzero"),
        // userfaultfd: Docker's default seccomp returns EPERM — a deterministic blocked-syscall probe.
        cc("weird/userfaultfd", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <linux/userfaultfd.h>
#include <sys/syscall.h>
#include <sys/ioctl.h>
#include <unistd.h>
int main(){
  int fd=syscall(SYS_userfaultfd,0);
  if(fd<0){perror("userfaultfd");return 1;}
  struct uffdio_api api={.api=UFFD_API};
  if(ioctl(fd,UFFDIO_API,&api)<0){perror("api");return 1;}
  printf("UFFD=ok\n"); return 0;
}"#).has("userfaultfd: Operation not permitted"),
        // io_uring_setup: also EPERM under Docker default seccomp.
        cc("weird/io-uring", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <linux/io_uring.h>
#include <unistd.h>
int main(){
  struct io_uring_params p; memset(&p,0,sizeof p);
  int fd=syscall(SYS_io_uring_setup,8,&p);
  if(fd<0){perror("io_uring_setup");return 1;}
  printf("IOURING=ok sq=%u\n",p.sq_entries); return 0;
}"#).has("io_uring_setup: Operation not permitted"),
        // bpf(BPF_MAP_CREATE): needs CAP_BPF — EPERM under default Docker.
        cc("weird/bpf-map-create", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <errno.h>
#include <string.h>
#include <sys/syscall.h>
#include <linux/bpf.h>
#include <unistd.h>
int main(){
  union bpf_attr a; memset(&a,0,sizeof a);
  a.map_type=BPF_MAP_TYPE_ARRAY; a.key_size=4; a.value_size=4; a.max_entries=1;
  long r=syscall(SYS_bpf,BPF_MAP_CREATE,&a,sizeof a);
  printf("BPF=%s\n", r<0?strerror(errno):"ok"); return 0;
}"#).has("BPF=Operation not permitted"),
        // seccomp(2): install a (allow-all) BPF filter — the sandboxing self-restriction path.
        cc("weird/seccomp-filter", "-O2", r#"#define _GNU_SOURCE
#include <stdio.h>
#include <linux/seccomp.h>
#include <linux/filter.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>
int main(){
  struct sock_filter f[]={ BPF_STMT(BPF_RET|BPF_K, SECCOMP_RET_ALLOW) };
  struct sock_fprog prog={.len=1,.filter=f};
  prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0);
  if(syscall(SYS_seccomp,SECCOMP_SET_MODE_FILTER,0,&prog)<0){perror("seccomp");return 1;}
  printf("SECCOMP=installed\n"); return 0;
}"#).has("SECCOMP=installed"),
    ]
}
