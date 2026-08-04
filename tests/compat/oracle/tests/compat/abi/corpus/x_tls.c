// Thread-local storage: FS-relative access on x86_64 (mov %fs:off), tpidr on aarch64.
// Exercises initial-exec + global-dynamic TLS models and per-thread isolation across pthreads.
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <pthread.h>
static __thread uint64_t ie_a = 0x1111111111111111ULL; // initial-exec (static, non-zero init)
static __thread uint64_t ie_b[8];
__thread uint64_t gd_c = 3;                              // global-dynamic (extern-visible)
static __thread uint32_t counter;

static uint64_t mix(uint64_t h, uint64_t v){ return h*1000003ULL + v; }

static void *worker(void *arg){
  uint64_t seed = (uint64_t)(uintptr_t)arg;
  uint64_t h = 0;
  ie_a = seed * 0x9E3779B97F4A7C15ULL;      // each thread owns its copy
  gd_c = seed + 7;
  counter = 0;
  for(int i=0;i<4096;i++){
    ie_b[i&7] += ie_a ^ (uint64_t)i;
    ie_a = ie_a*6364136223846793005ULL + 1442695040888963407ULL;
    gd_c += ie_b[i&7];
    counter++;
  }
  for(int i=0;i<8;i++) h = mix(h, ie_b[i]);
  h = mix(h, ie_a); h = mix(h, gd_c); h = mix(h, counter);
  uint64_t *out = malloc(sizeof(uint64_t)); *out = h; return out;
}

int main(void){
  uint64_t h = 0;
  h = mix(h, ie_a); h = mix(h, gd_c);
  pthread_t t[4];
  for(uint64_t i=0;i<4;i++) pthread_create(&t[i], NULL, worker, (void*)(uintptr_t)(i+1));
  for(int i=0;i<4;i++){ void *r; pthread_join(t[i], &r); h = mix(h, *(uint64_t*)r); free(r); }
  // main-thread TLS untouched by workers
  h = mix(h, ie_a); h = mix(h, gd_c); h = mix(h, counter);
  printf("tls=%016llx\n",(unsigned long long)h);
  return 0;
}
