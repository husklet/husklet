#include "compat.h"
#include <stdio.h>
#include <errno.h>
/* close_range with an unknown flag bit must fail EINVAL and touch no fd (Linux rejects unknown flags). */
int main(void){
  long r=syscall(__NR_close_range, 3u, 3u, (unsigned)0x80000000u);
  printf("close_range_flags einval=%d\n", (r==-1 && errno==EINVAL)); return 0; }
