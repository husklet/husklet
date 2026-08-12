#include <stddef.h>
#include <stdint.h>

static volatile uint64_t data_value = UINT64_C(0x13579bdf2468ace0);
static volatile uint64_t *data_pointer = &data_value;

__attribute__((noinline)) static uint64_t direct_call(uint64_t value) {
  return value ^ UINT64_C(0x55aa55aa55aa55aa);
}

__attribute__((noinline)) static uint64_t indirect_call(uint64_t value) {
  return value + UINT64_C(0x102030405060708);
}

static uint64_t (*volatile function_pointer)(uint64_t) = indirect_call;

__attribute__((noinline)) static uintptr_t current_pc(void) {
  uintptr_t pc;
#if defined(__x86_64__)
  __asm__ volatile("lea 0(%%rip), %0" : "=r"(pc));
#elif defined(__aarch64__)
  __asm__ volatile("adr %0, ." : "=r"(pc));
#else
#error unsupported guest architecture
#endif
  return pc;
}

static long write_stdout(const void *bytes, size_t size) {
#if defined(__x86_64__)
  register long number __asm__("rax") = 1;
  register long descriptor __asm__("rdi") = 1;
  register const void *buffer __asm__("rsi") = bytes;
  register size_t count __asm__("rdx") = size;
  __asm__ volatile("syscall"
                   : "+a"(number)
                   : "D"(descriptor), "S"(buffer), "d"(count)
                   : "rcx", "r11", "memory");
  return number;
#else
  register long descriptor __asm__("x0") = 1;
  register const void *buffer __asm__("x1") = bytes;
  register size_t count __asm__("x2") = size;
  register long number __asm__("x8") = 64;
  __asm__ volatile("svc 0"
                   : "+r"(descriptor)
                   : "r"(buffer), "r"(count), "r"(number)
                   : "memory");
  return descriptor;
#endif
}

__attribute__((noreturn)) static void exit_guest(int status) {
#if defined(__x86_64__)
  register long number __asm__("rax") = 60;
  register long code __asm__("rdi") = status;
  __asm__ volatile("syscall"
                   :
                   : "a"(number), "D"(code)
                   : "rcx", "r11", "memory");
#else
  register long code __asm__("x0") = status;
  register long number __asm__("x8") = 93;
  __asm__ volatile("svc 0" : : "r"(code), "r"(number) : "memory");
#endif
  __builtin_unreachable();
}

void _start(void) {
  static const char success[] = "displaced-et-exec-ok\n";
  uintptr_t pc = current_pc();
  uintptr_t function = (uintptr_t)current_pc;
  int valid = pc >= function && pc - function < 128;
  valid &= function < UINT64_C(0x10000000);
  valid &= (uintptr_t)&data_value < UINT64_C(0x10000000);
  valid &= data_pointer == &data_value &&
           *data_pointer == UINT64_C(0x13579bdf2468ace0);
  valid &= direct_call(7) == (UINT64_C(7) ^ UINT64_C(0x55aa55aa55aa55aa));
  valid &= function_pointer(9) == UINT64_C(9) + UINT64_C(0x102030405060708);
  valid &=
      write_stdout(success, sizeof(success) - 1) == (long)(sizeof(success) - 1);
  exit_guest(valid ? 0 : 1);
}
