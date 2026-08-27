#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <pthread.h>

struct answer {
    uint64_t mov_rax, mov_r15, sub_rdx, flags;
};

extern long fs_tls_case(void *, struct answer *);

struct thread_case {
    unsigned char tls[64];
    struct answer answer;
};

static void *thread_main(void *opaque) {
    struct thread_case *test = opaque;
    return (void *)(uintptr_t)fs_tls_case(test->tls, &test->answer);
}

__asm__(".text\n"
        ".global fs_tls_case\n"
        "fs_tls_case:\n"
        " push %r12; push %r13; push %r15; sub $16,%rsp\n"
        " mov %rdi,%r12; mov %rsi,%r13\n"
        " mov $0x1003,%edi; mov %rsp,%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 9f\n"
        " mov $0x1002,%edi; mov %r12,%rsi; mov $158,%eax; syscall; test %rax,%rax; jne 9f\n"
        " jmp 1f\n"
        "1: .byte 0x64,0x48,0x8b,0x04,0x25,0x20,0,0,0\n"
        " mov %rax,0(%r13); jmp 2f\n"
        "2: .byte 0x64,0x4c,0x8b,0x3c,0x25,0x28,0,0,0\n"
        " mov %r15,8(%r13); mov $0xfedcba9876543210,%rdx; jmp 3f\n"
        "3: .byte 0x64,0x48,0x2b,0x14,0x25,0x30,0,0,0\n"
        " pushfq; pop %rax; mov %rdx,16(%r13); mov %rax,24(%r13)\n"
        " mov $0x1002,%edi; mov (%rsp),%rsi; mov $158,%eax; syscall; jmp 8f\n"
        "9: mov $-1,%rax\n"
        "8: add $16,%rsp; pop %r15; pop %r13; pop %r12; ret\n");

int main(void) {
    unsigned char tls[64] = {0};
    struct answer answer = {0};
    *(uint64_t *)(tls + 0x20) = UINT64_C(0x0123456789abcdef);
    *(uint64_t *)(tls + 0x28) = UINT64_C(0xf0e1d2c3b4a59687);
    *(uint64_t *)(tls + 0x30) = UINT64_C(0x1111111111111111);
    long rc = fs_tls_case(tls, &answer);
    struct thread_case threads[2] = {0};
    for (unsigned i = 0; i < 2; i++) {
        *(uint64_t *)(threads[i].tls + 0x20) = UINT64_C(0x1010101010101010) + i;
        *(uint64_t *)(threads[i].tls + 0x28) = UINT64_C(0x2020202020202020) + i;
        *(uint64_t *)(threads[i].tls + 0x30) = UINT64_C(0x1111111111111111) + i;
    }
    pthread_t ids[2];
    int thread_ok = pthread_create(&ids[0], NULL, thread_main, &threads[0]) == 0 &&
                    pthread_create(&ids[1], NULL, thread_main, &threads[1]) == 0;
    for (unsigned i = 0; i < 2 && thread_ok; i++) {
        void *result;
        thread_ok = pthread_join(ids[i], &result) == 0 && result == NULL &&
                    threads[i].answer.mov_rax == *(uint64_t *)(threads[i].tls + 0x20) &&
                    threads[i].answer.mov_r15 == *(uint64_t *)(threads[i].tls + 0x28);
    }
    printf("fs rc=%ld mov=%016llx high=%016llx sub=%016llx flags=%04llx threads=%d\n", rc,
           (unsigned long long)answer.mov_rax, (unsigned long long)answer.mov_r15,
           (unsigned long long)answer.sub_rdx, (unsigned long long)(answer.flags & UINT64_C(0xcd5)), thread_ok);
    return rc == 0 && answer.mov_rax == UINT64_C(0x0123456789abcdef) &&
                   answer.mov_r15 == UINT64_C(0xf0e1d2c3b4a59687) &&
                   answer.sub_rdx == UINT64_C(0xedcba987654320ff) && thread_ok
               ? 0
               : 3;
}
