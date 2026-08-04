// TLS access-model coverage: one __thread variable per aarch64/x86 access model (local-exec,
// initial-exec, general-dynamic, local-dynamic), each set then re-read after a churn of allocations,
// on BOTH the main thread and a spawned thread (the CLONE_SETTLS path). A pointer's identity must
// survive the intervening allocations -- the exact pattern behind DB::current_thread in clickhouse
// (#281), whose re-entrancy guard was suspected to read a stale/NULL thread_local. Guards that every
// TLS relocation/model hl translates resolves to the same address the native oracle uses. Portable
// (golden), and paired with a non-PIE ET_EXEC build (the local-exec model clickhouse actually uses).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
/* one variable per access model, selected via attribute (works x86 & arm) */
__thread void *v_le __attribute__((tls_model("local-exec")));
__thread void *v_ie __attribute__((tls_model("initial-exec")));
__thread void *v_gd __attribute__((tls_model("global-dynamic")));
__thread void *v_ld __attribute__((tls_model("local-dynamic")));
__thread long  n_le __attribute__((tls_model("local-exec")));
static void churn(void){ for(int i=0;i<1500;i++){void*m=malloc(48+(i&63));memset(m,i,16);if(i&1)free(m);} }
static int check(const char*who){
  long m0,m1,m2,m3;
  v_le=&m0; v_ie=&m1; v_gd=&m2; v_ld=&m3; n_le=0x5A5A;
  churn();
  int ok = (v_le==&m0)+(v_ie==&m1)+(v_gd==&m2)+(v_ld==&m3)+(n_le==0x5A5A);
  return ok; /* expect 5 */
}
static int rc_main, rc_thr;
static void* worker(void*a){ (void)a; rc_thr=check("thread"); return 0; }
int main(void){
  rc_main=check("main");
  pthread_t t; pthread_create(&t,0,worker,0); pthread_join(t,0);
  printf("tlsmodels main=%d thread=%d\n", rc_main, rc_thr); /* 5 5 */
  return (rc_main==5 && rc_thr==5)?0:1;
}
