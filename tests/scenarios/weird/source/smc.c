#include <stdio.h>
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
}
