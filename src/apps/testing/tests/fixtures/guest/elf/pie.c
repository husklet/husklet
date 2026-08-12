#include <stddef.h>
#include <stdint.h>

static volatile uint64_t value = UINT64_C(0x13579bdf2468ace0);

__attribute__((noinline)) static uint64_t direct(uint64_t input) {
  return input ^ UINT64_C(0x55aa55aa55aa55aa);
}

__attribute__((noinline)) static uint64_t indirect(uint64_t input) {
  return input + UINT64_C(0x102030405060708);
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
#elif defined(__aarch64__)
  register long descriptor __asm__("x0") = 1;
  register const void *buffer __asm__("x1") = bytes;
  register size_t count __asm__("x2") = size;
  register long number __asm__("x8") = 64;
  __asm__ volatile("svc 0"
                   : "+r"(descriptor)
                   : "r"(buffer), "r"(count), "r"(number)
                   : "memory");
  return descriptor;
#else
#error unsupported guest architecture
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
  static const char success[] = "pie-exec-ok\n";
  volatile uint64_t *pointer = &value;
  uint64_t (*volatile function)(uint64_t) = indirect;
  int valid = pointer == &value && *pointer == UINT64_C(0x13579bdf2468ace0);
  valid &= direct(7) == (UINT64_C(7) ^ UINT64_C(0x55aa55aa55aa55aa));
  valid &= function(9) == UINT64_C(9) + UINT64_C(0x102030405060708);
  valid &=
      write_stdout(success, sizeof(success) - 1) == (long)(sizeof(success) - 1);
  exit_guest(valid ? 0 : 1);
}
