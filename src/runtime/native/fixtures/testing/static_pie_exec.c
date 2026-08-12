#include <stdint.h>
#include <stdio.h>

static volatile uint64_t value = UINT64_C(0x13579bdf2468ace0);
static volatile uint64_t *pointer = &value;

__attribute__((noinline)) static uint64_t direct(uint64_t input) {
  return input ^ UINT64_C(0x55aa55aa55aa55aa);
}

__attribute__((noinline)) static uint64_t indirect(uint64_t input) {
  return input + UINT64_C(0x102030405060708);
}

static uint64_t (*volatile function)(uint64_t) = indirect;

int main(void) {
  int valid = pointer == &value && *pointer == UINT64_C(0x13579bdf2468ace0);
  valid &= direct(7) == (UINT64_C(7) ^ UINT64_C(0x55aa55aa55aa55aa));
  valid &= function(9) == UINT64_C(9) + UINT64_C(0x102030405060708);
  if (!valid)
    return 1;
  return puts("pie-exec-ok") < 0;
}
