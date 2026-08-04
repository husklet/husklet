// glibc SSE4.2/AVX string search routines (strstr/strchr/strrchr/strspn/strcspn/strpbrk/strcasestr):
// statically linked glibc uses PCMPISTRI/PCMPEQB etc, so the translator must handle them.
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <stdlib.h>
static uint64_t mix(uint64_t h, uint64_t v){ return h*1000003ULL + v; }
int main(void){
  static char buf[8192];
  uint64_t r = 88172645463325252ULL, h = 0;
  for(size_t i=0;i<sizeof(buf)-1;i++){
    r ^= r<<13; r ^= r>>7; r ^= r<<17;
    buf[i] = (char)('a' + (r % 26));
  }
  buf[sizeof(buf)-1]=0;
  const char *needles[] = {"abc","xyz","qq","mnop","zzzz","the","aaa","lm"};
  for(int n=0;n<8;n++){
    const char *p = strstr(buf, needles[n]);
    h = mix(h, p ? (uint64_t)(p-buf) : 0xffffffffULL);
    h = mix(h, (uint64_t)strspn(buf, "aeiou"));
    h = mix(h, (uint64_t)strcspn(buf, needles[n]));
    char *pb = strpbrk(buf, needles[n]);
    h = mix(h, pb ? (uint64_t)(pb-buf) : 0xffffffffULL);
  }
  for(char c='a'; c<='z'; c++){
    char *f = strchr(buf, c), *l = strrchr(buf, c);
    h = mix(h, f?(uint64_t)(f-buf):0); h = mix(h, l?(uint64_t)(l-buf):0);
    h = mix(h, (uint64_t)strlen(buf));
  }
  printf("search=%016llx\n",(unsigned long long)h);
  return 0;
}
