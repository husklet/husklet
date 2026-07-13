#define _GNU_SOURCE
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>
#include <stdio.h>
#include <stdint.h>
#include <errno.h>
#include <string.h>
#include <time.h>
static int send_fd(int s,int fd){char d='x';struct iovec io={&d,1};char cb[CMSG_SPACE(sizeof(int))];memset(cb,0,sizeof cb);
 struct msghdr m={0};m.msg_iov=&io;m.msg_iovlen=1;m.msg_control=cb;m.msg_controllen=sizeof cb;
 struct cmsghdr*c=CMSG_FIRSTHDR(&m);c->cmsg_level=SOL_SOCKET;c->cmsg_type=SCM_RIGHTS;c->cmsg_len=CMSG_LEN(sizeof(int));
 memcpy(CMSG_DATA(c),&fd,sizeof(int));return sendmsg(s,&m,0)<0?-1:0;}
static int recv_fd(int s){char d;struct iovec io={&d,1};char cb[CMSG_SPACE(sizeof(int))];memset(cb,0,sizeof cb);
 struct msghdr m={0};m.msg_iov=&io;m.msg_iovlen=1;m.msg_control=cb;m.msg_controllen=sizeof cb;
 if(recvmsg(s,&m,0)<0)return -1;struct cmsghdr*c=CMSG_FIRSTHDR(&m);if(!c||c->cmsg_type!=SCM_RIGHTS)return -1;
 int fd;memcpy(&fd,CMSG_DATA(c),sizeof(int));return fd;}
int main(void){
#if 1
    for(int i=0;i<1500;i++) if(dup(2)<0) break;
#endif
    int efd=eventfd(0,0);
    int sv[2]; socketpair(AF_UNIX,SOCK_STREAM,0,sv);
    pid_t p=fork();
    if(p==0){ close(sv[0]); int r=recv_fd(sv[1]);
        struct timespec ts={0,200*1000*1000}; nanosleep(&ts,NULL);
        uint64_t v=5; ssize_t w=write(r,&v,8); _exit(w==8?0:1);}
    close(sv[1]); send_fd(sv[0],efd);
    uint64_t got=0; ssize_t rr=read(efd,&got,8); int e=errno;
    int st; waitpid(p,&st,0);
    printf("efd_scm r=%zd got=%llu errno=%d child=%d\n",rr,(unsigned long long)got,rr<0?e:0,WIFEXITED(st)?WEXITSTATUS(st):-1);
    return 0;}
