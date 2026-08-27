#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <pthread.h>
#include <sys/mman.h>
#include <unistd.h>

struct answer {
    uint64_t mov_rax, mov_r15, sub_rdx, flags, mov_r11, kept_r10, negative;
};

extern long fs_tls_case(void *, struct answer *);

struct thread_case {
    unsigned char *mapping, *tls;
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
        "1: .byte 0x64,0x48,0x8b,0x04,0x25,0x40,0x23,0x01,0\n"
        " mov %rax,0(%r13); jmp 2f\n"
        "2: .byte 0x64,0x4c,0x8b,0x3c,0x25,0x48,0x23,0x01,0\n"
        " mov %r15,8(%r13); mov $0xfedcba9876543210,%rdx; jmp 3f\n"
        "3: .byte 0x64,0x48,0x2b,0x14,0x25,0x50,0x23,0x01,0\n"
        " pushfq; pop %rax; mov %rdx,16(%r13); mov %rax,24(%r13)\n"
        " mov $0x1020304050607080,%r10; mov $0x8877665544332211,%r11; jmp 4f\n"
        "4: .byte 0x64,0x4c,0x8b,0x1c,0x25,0x58,0x23,0x01,0\n"
        " mov %r11,32(%r13); mov %r10,40(%r13); jmp 5f\n"
        "5: .byte 0x64,0x48,0x8b,0x0c,0x25,0xc0,0xdc,0xff,0xff\n"
        " mov %rcx,48(%r13)\n"
        " mov $0x1002,%edi; mov (%rsp),%rsi; mov $158,%eax; syscall; jmp 8f\n"
        "9: mov $-1,%rax\n"
        "8: add $16,%rsp; pop %r15; pop %r13; pop %r12; ret\n");

int main(int argc, char **argv) {
    (void)argv;
    int authority = argc > 1;
    if (authority) {
        size_t page = (size_t)sysconf(_SC_PAGESIZE);
        if (mmap(NULL, page, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED)
            return 2;
    }
    size_t tls_size = 0x20000;
    unsigned char *mapping = mmap(NULL, tls_size, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mapping == MAP_FAILED) return 2;
    unsigned char *tls = mapping + 0x4000;
    struct answer answer = {0};
    *(uint64_t *)(tls + 0x12340) = UINT64_C(0x0123456789abcdef);
    *(uint64_t *)(tls + 0x12348) = UINT64_C(0xf0e1d2c3b4a59687);
    *(uint64_t *)(tls + 0x12350) = UINT64_C(0x1111111111111111);
    *(uint64_t *)(tls + 0x12358) = UINT64_C(0x13579bdf2468ace0);
    *(uint64_t *)(tls - 0x2340) = UINT64_C(0x55aa33cc77ee11dd);
    long rc = fs_tls_case(tls, &answer);
    struct thread_case threads[2] = {0};
    for (unsigned i = 0; i < 2; i++) {
        threads[i].mapping = mmap(NULL, tls_size, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (threads[i].mapping == MAP_FAILED) return 2;
        threads[i].tls = threads[i].mapping + 0x4000;
        *(uint64_t *)(threads[i].tls + 0x12340) = UINT64_C(0x1010101010101010) + i;
        *(uint64_t *)(threads[i].tls + 0x12348) = UINT64_C(0x2020202020202020) + i;
        *(uint64_t *)(threads[i].tls + 0x12350) = UINT64_C(0x1111111111111111) + i;
        *(uint64_t *)(threads[i].tls + 0x12358) = UINT64_C(0x3030303030303030) + i;
        *(uint64_t *)(threads[i].tls - 0x2340) = UINT64_C(0x4040404040404040) + i;
    }
    pthread_t ids[2];
    int thread_ok = pthread_create(&ids[0], NULL, thread_main, &threads[0]) == 0 &&
                    pthread_create(&ids[1], NULL, thread_main, &threads[1]) == 0;
    for (unsigned i = 0; i < 2 && thread_ok; i++) {
        void *result;
        thread_ok = pthread_join(ids[i], &result) == 0 && result == NULL &&
                    threads[i].answer.mov_rax == *(uint64_t *)(threads[i].tls + 0x12340) &&
                    threads[i].answer.mov_r15 == *(uint64_t *)(threads[i].tls + 0x12348) &&
                    threads[i].answer.mov_r11 == *(uint64_t *)(threads[i].tls + 0x12358) &&
                    threads[i].answer.kept_r10 == UINT64_C(0x1020304050607080) &&
                    threads[i].answer.negative == *(uint64_t *)(threads[i].tls - 0x2340);
    }
    printf("fs rc=%ld mov=%016llx high=%016llx sub=%016llx flags=%04llx r11=%016llx r10=%016llx"
           " neg=%016llx threads=%d authority=%d\n", rc,
           (unsigned long long)answer.mov_rax, (unsigned long long)answer.mov_r15,
           (unsigned long long)answer.sub_rdx, (unsigned long long)(answer.flags & UINT64_C(0xcd5)),
           (unsigned long long)answer.mov_r11, (unsigned long long)answer.kept_r10,
           (unsigned long long)answer.negative, thread_ok, authority);
    return rc == 0 && answer.mov_rax == UINT64_C(0x0123456789abcdef) &&
                   answer.mov_r15 == UINT64_C(0xf0e1d2c3b4a59687) &&
                   answer.sub_rdx == UINT64_C(0xedcba987654320ff) &&
                   answer.mov_r11 == UINT64_C(0x13579bdf2468ace0) &&
                   answer.kept_r10 == UINT64_C(0x1020304050607080) &&
                   answer.negative == UINT64_C(0x55aa33cc77ee11dd) && thread_ok
               ? 0
               : 3;
}
