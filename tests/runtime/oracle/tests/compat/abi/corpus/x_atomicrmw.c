// LOCK xadd/cmpxchg/and/or/xor + xchg atomics single-threaded correctness of returned prior value.
#include <stdio.h>
#include <stdint.h>
int main(void){
  uint64_t h=0; uint64_t a=0; uint32_t b=0;
  for(int i=0;i<20000;i++){
    uint64_t prev=__atomic_fetch_add(&a,(uint64_t)i*3+1,__ATOMIC_SEQ_CST);
    uint32_t po=__atomic_fetch_or(&b,(uint32_t)(1u<<(i&31)),__ATOMIC_SEQ_CST);
    uint32_t px=__atomic_fetch_xor(&b,(uint32_t)(i*2654435761u),__ATOMIC_SEQ_CST);
    uint64_t exp=prev, des=prev^0x5A5A5A5AULL;
    int ok=__atomic_compare_exchange_n(&a,&exp,des,0,__ATOMIC_SEQ_CST,__ATOMIC_SEQ_CST);
    uint32_t xg=__atomic_exchange_n(&b,(uint32_t)i,__ATOMIC_SEQ_CST);
    h=h*1000003ULL+prev+po+px+(unsigned)ok+xg;
  }
  printf("atom=%016llx a=%016llx b=%08x\n",(unsigned long long)h,(unsigned long long)a,b);
  return 0;
}
