// #145 flag residuals: shift/rotate/imul/mul flags byte-exact vs qemu. Each case executes one
// instruction, captures RFLAGS via pushfq, and prints ONLY the architecturally-DEFINED flag bits
// for that op (so the oracle diff is meaningful, not a probe of qemu's "undefined" lazy-flag model):
//   SHL/SHR/SAR: CF,SF,ZF,PF always + OF iff count==1   (AF undefined)
//   ROL/ROR/RCL/RCR: CF always + OF iff count==1        (SF/ZF/PF/AF unchanged)
//   IMUL/MUL: CF,OF only                                (SF/ZF/PF/AF undefined)
#include <stdio.h>
#include <stdint.h>
// bit values: O=0x800 S=0x80 Z=0x40 A=0x10 P=0x04 C=0x01
static char BUF[8];
static const char *fs(uint64_t f, unsigned show){
  int i=0;
  if(show&0x800) BUF[i++]=(f&0x800)?'O':'-';
  if(show&0x80)  BUF[i++]=(f&0x80)?'S':'-';
  if(show&0x40)  BUF[i++]=(f&0x40)?'Z':'-';
  if(show&0x04)  BUF[i++]=(f&0x04)?'P':'-';
  if(show&0x01)  BUF[i++]=(f&0x01)?'C':'-';
  BUF[i]=0; return BUF;
}
#define SHOW_S1 (0x800|0x80|0x40|0x04|0x01) /* shift count==1 */
#define SHOW_SN (0x80|0x40|0x04|0x01)       /* shift count>1  */
#define SHOW_R1 (0x800|0x01)                /* rotate count==1 */
#define SHOW_RN (0x01)                      /* rotate count>1  */
#define SHOW_MUL (0x800|0x01)               /* imul/mul */
#define PR(label,res,show) printf("%-14s r=%016lx %s\n", label,(unsigned long)(res),fs(_f,show))

int main(void){
  uint64_t _f; unsigned long v; unsigned char cl;
  #define SH1(name,mn,init,rmask,show) v=init; __asm__ volatile(mn "\n\tpushfq\n\tpop %1":"+q"(v),"=r"(_f)::"cc"); PR(name, v&rmask, show)
  #define SH1R(name,mn,init,rmask,show) v=init; __asm__ volatile(mn "\n\tpushfq\n\tpop %1":"+r"(v),"=r"(_f)::"cc"); PR(name, v&rmask, show)
  #define SHCL(name,mn,init,c,rmask,show) v=init; cl=c; __asm__ volatile(mn "\n\tpushfq\n\tpop %1":"+r"(v),"=r"(_f):"c"(cl):"cc"); PR(name, v&rmask, show)
  // ---- SHL/SHR/SAR r/m8 by 1 (D0 form): defined OF (count==1) ----
  SH1("shlb1_00","shlb %b0",0x00,0xff,SHOW_S1);
  SH1("shlb1_80","shlb %b0",0x80,0xff,SHOW_S1);
  SH1("shlb1_01","shlb %b0",0x01,0xff,SHOW_S1);
  SH1("shlb1_40","shlb %b0",0x40,0xff,SHOW_S1);
  SH1("shlb1_C0","shlb %b0",0xC0,0xff,SHOW_S1);
  SH1("shrb1_00","shrb %b0",0x00,0xff,SHOW_S1);
  SH1("shrb1_01","shrb %b0",0x01,0xff,SHOW_S1);
  SH1("shrb1_03","shrb %b0",0x03,0xff,SHOW_S1);
  SH1("sarb1_80","sarb %b0",0x80,0xff,SHOW_S1);
  SH1("sarb1_81","sarb %b0",0x81,0xff,SHOW_S1);
  // ---- 16-bit by 1 (D1) ----
  SH1R("shlw1_8000","shlw %w0",0x8000,0xffff,SHOW_S1);
  SH1R("shlw1_4000","shlw %w0",0x4000,0xffff,SHOW_S1);
  SH1R("shrw1_0001","shrw %w0",0x0001,0xffff,SHOW_S1);
  SH1R("sarw1_8001","sarw %w0",0x8001,0xffff,SHOW_S1);
  // ---- 32/64-bit by 1 ----
  SH1R("shll1_msb","shll %k0",0x80000000UL,0xffffffff,SHOW_S1);
  SH1R("shll1_40", "shll %k0",0x40000000UL,0xffffffff,SHOW_S1);
  SH1R("shlq1_msb","shlq %0",0x8000000000000000UL,~0UL,SHOW_S1);
  SH1R("sarq1_msb","sarq %0",0x8000000000000000UL,~0UL,SHOW_S1);
  // ---- by immediate (C0/C1), count>1: OF undefined -> not shown ----
  SH1("shlb_i3","shlb $3,%b0",0x81,0xff,SHOW_SN);
  SH1("shrb_i4","shrb $4,%b0",0xFF,0xff,SHOW_SN);
  SH1("shlb_i8","shlb $8,%b0",0x81,0xff,SHOW_SN);   // count==width: byte fully out
  SH1("shrb_i9","shrb $9,%b0",0xFF,0xff,SHOW_SN);   // count>width
  SH1R("shlw_i5","shlw $5,%w0",0x1234,0xffff,SHOW_SN);
  SH1R("shll_i7","shll $7,%k0",0x12345678UL,0xffffffff,SHOW_SN);
  SH1R("sarl_i2","sarl $2,%k0",0x80000001UL,0xffffffff,SHOW_SN);
  SH1R("sarb_i9","sarb $9,%b0",0x80,0xff,SHOW_SN);  // SAR count>width keeps sign in CF
  // ---- by CL ----
  SHCL("shlb_cl1","shlb %%cl,%b0",0x01,1,0xff,SHOW_S1);
  SHCL("shrb_cl1","shrb %%cl,%b0",0x80,1,0xff,SHOW_S1);
  SHCL("shrb_cl4","shrb %%cl,%b0",0x80,4,0xff,SHOW_SN);
  SHCL("sarb_cl1","sarb %%cl,%b0",0x81,1,0xff,SHOW_S1);
  SHCL("shll_cl5","shll %%cl,%k0",0x12345678UL,5,0xffffffff,SHOW_SN);
  SHCL("shll_cl1","shll %%cl,%k0",0x80000000UL,1,0xffffffff,SHOW_S1);
  SHCL("sarl_cl1","sarl %%cl,%k0",0x80000001UL,1,0xffffffff,SHOW_S1);
  SHCL("shlw_cl1","shlw %%cl,%w0",0x4000,1,0xffff,SHOW_S1);
  // ---- ROL/ROR (rotate): CF always, OF iff count==1 ----
  SH1("rolb1_80","rolb %b0",0x80,0xff,SHOW_R1);
  SH1("rorb1_01","rorb %b0",0x01,0xff,SHOW_R1);
  SH1("rolb1_40","rolb %b0",0x40,0xff,SHOW_R1);
  SH1("rolb_i3","rolb $3,%b0",0x81,0xff,SHOW_RN);
  SH1("rorb_i2","rorb $2,%b0",0x81,0xff,SHOW_RN);
  SH1R("rolw1","rolw %w0",0x8001,0xffff,SHOW_R1);
  SHCL("roll_cl3","roll %%cl,%k0",0x12345678UL,3,0xffffffff,SHOW_RN);
  SHCL("rorl_cl1","rorl %%cl,%k0",0x12345678UL,1,0xffffffff,SHOW_R1);
  SHCL("rorb_cl1","rorb %%cl,%b0",0x1,1,0xff,SHOW_R1);
  SHCL("rolb_cl1","rolb %%cl,%b0",0x80,1,0xff,SHOW_R1);
  SH1R("roll_i4","roll $4,%k0",0x12345678UL,0xffffffff,SHOW_RN);
  // ---- RCL/RCR (carry-in cleared first) ----
  SH1("rclb1_80","clc; rclb %b0",0x80,0xff,SHOW_R1);
  SH1("rcrb1_01","clc; rcrb %b0",0x01,0xff,SHOW_R1);
  // ---- IMUL 2-operand: CF/OF only ----
  #define IM2(name,ty,mn,ia,ib) { ty a=ia,b=ib; __asm__ volatile(mn "\n\tpushfq\n\tpop %1":"+r"(a),"=r"(_f):"r"(b):"cc"); PR(name,(unsigned long)a,SHOW_MUL); }
  IM2("imulq_big",long,"imulq %2,%0",0x100000000L,2L);
  IM2("imull_of",int,"imull %2,%0",0x10000,0x10000);
  IM2("imull_sm",int,"imull %2,%0",3,5);
  IM2("imulw_of",short,"imulw %2,%0",0x100,0x100);
  IM2("imull_neg",int,"imull %2,%0",-2,3);
  IM2("imulq_neg",long,"imulq %2,%0",-2L,3L);
  // ---- IMUL 3-operand imm: CF/OF only ----
  { int a=0x40000000,r; __asm__ volatile("imull $4,%2,%0\n\tpushfq\n\tpop %1":"=r"(r),"=r"(_f):"r"(a):"cc"); PR("imul3_of",(unsigned long)(unsigned)r,SHOW_MUL); }
  { int a=7,r; __asm__ volatile("imull $3,%2,%0\n\tpushfq\n\tpop %1":"=r"(r),"=r"(_f):"r"(a):"cc"); PR("imul3_ok",(unsigned long)(unsigned)r,SHOW_MUL); }
  // ---- MUL unsigned one-operand: CF/OF only ----
  { unsigned long a=0x100000000UL,d; __asm__ volatile("mulq %2\n\tpushfq\n\tpop %0":"=r"(_f),"+a"(a),"=d"(d):"r"(2UL):"cc"); PR("mulq_big",d,SHOW_MUL); }
  { unsigned int a=3,d; __asm__ volatile("mull %2\n\tpushfq\n\tpop %0":"=r"(_f),"+a"(a),"=d"(d):"r"(5U):"cc"); PR("mull_small",a,SHOW_MUL); }
  return 0;
}
