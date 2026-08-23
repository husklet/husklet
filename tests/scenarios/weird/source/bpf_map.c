#define _GNU_SOURCE
#include <stdio.h>
#include <errno.h>
#include <string.h>
#include <sys/syscall.h>
#include <linux/bpf.h>
#include <unistd.h>

int main() {
    union bpf_attr a;
    memset(&a, 0, sizeof a);
    a.map_type = BPF_MAP_TYPE_ARRAY;
    a.key_size = 4;
    a.value_size = 4;
    a.max_entries = 1;
    long r = syscall(SYS_bpf, BPF_MAP_CREATE, &a, sizeof a);
    printf("BPF=%s\n", r < 0 ? strerror(errno) : "ok");
    return 0;
}
