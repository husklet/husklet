#include <stdio.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
int main(void){ int fd=open("/tmp/hlstat",O_CREAT|O_WRONLY|O_TRUNC,0644); write(fd,"abcd",4); close(fd);
  struct stat st; stat("/tmp/hlstat",&st); int acc=access("/tmp/hlstat",R_OK); unlink("/tmp/hlstat");
  printf("stat size=%lld reg=%d acc=%d\n",(long long)st.st_size,(int)(S_ISREG(st.st_mode)!=0),acc); return 0; }
