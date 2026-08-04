// TIOCNOTTY drops the controlling terminal: a session leader that acquired a pty as its ctty can
// reach /dev/tty, and after TIOCNOTTY the ctty is gone so /dev/tty returns ENXIO. SIGHUP is ignored
// so the leader survives the hangup TIOCNOTTY delivers to its own (foreground) group.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <termios.h>
#include <unistd.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <errno.h>
static char name[128];
int main(void){
  int m=posix_openpt(O_RDWR|O_NOCTTY); grantpt(m); unlockpt(m);
  strncpy(name, ptsname(m), sizeof name-1);
  int p[2]; pipe(p);
  pid_t a=fork();
  if(a==0){
    signal(SIGHUP,SIG_IGN);
    setsid();
    int s=open(name,O_RDWR); ioctl(s,TIOCSCTTY,0);
    int dt1=open("/dev/tty",O_RDWR); int had=(dt1>=0); if(dt1>=0)close(dt1);
    int nr=ioctl(s,TIOCNOTTY,0);
    int dt2=open("/dev/tty",O_RDWR); int dropped=(dt2<0&&errno==ENXIO); if(dt2>=0)close(dt2);
    int r=(had?1:0)|(dropped?2:0)|(nr==0?4:0);
    write(p[1],&r,sizeof r); _exit(0);
  }
  close(p[1]); int ra=0; read(p[0],&ra,sizeof ra); int st; waitpid(a,&st,0);
  printf("notty had=%d drop_enxio=%d notty_ok=%d\n",(ra&1)!=0,(ra&2)!=0,(ra&4)!=0);
  close(m); return 0;
}
