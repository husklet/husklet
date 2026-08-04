#include <stdio.h>
#include <sys/eventfd.h>
#include <unistd.h>
#include <stdint.h>
int main(){
  int fd=eventfd(0,0);
  uint64_t v=41; write(fd,&v,8); v=1; write(fd,&v,8);
  uint64_t r; read(fd,&r,8);
  printf("EVENTFD=%llu\n",(unsigned long long)r); return 0;
}
