typedef unsigned long usize;
static long call1(long n,long a){long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a):"rcx","r11","memory");return r;}
static long call3(long n,long a,long b,long c){long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
static long call4(long n,long a,long b,long c,long d){long r;register long r10 __asm__("r10")=d;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");return r;}
static int same(const char*a,const char*b,usize n){for(usize i=0;i<n;i++)if(a[i]!=b[i])return 0;return 1;}
void _start(void){
 char data[16],link[32]; long fd=call4(257,-100,(long)"/data",0,0);
 if(fd<0||call3(0,fd,(long)data,8)!=8||!same(data,"original",8))call1(60,1); call1(3,fd);
 long count=call4(267,-100,(long)"/proc/self/exe",(long)link,32);
 if(count!=6||!same(link,"/guest",6))call1(60,2);
 call3(1,1,(long)"projected-file-ok\n",18);call1(60,0);
}
