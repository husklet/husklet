#include <stdio.h>
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
}
