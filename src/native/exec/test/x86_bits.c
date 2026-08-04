#include "../src/arch/x86_64/frontend.h"
#include "../src/arch/x86_64/entry.h"
#include "../include/cpu.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "x86_bits:%d: %s\n", __LINE__, #x); return __LINE__; } } while (0)

#if defined(__aarch64__)
extern void hl_x86_test_enter(hl_native_x86_64_cpu *, void *);
#endif

static hl_x86_a64_status emit(const uint8_t *bytes, size_t size, uint32_t *host,
                              hl_x86_a64_result *result) {
    hl_x86_a64_provenance provenance[2] = {0};
    hl_x86_a64_request request = {
        .abi = HL_X86_A64_FRONTEND_ABI, .size = sizeof(request), .guest_pc = 0x400000,
        .guest_bytes = bytes, .guest_size = size, .max_instructions = 1,
        .host_words = host, .host_capacity = 512, .provenance = provenance,
        .provenance_capacity = 2,
    };
    return hl_x86_a64_emit(&request, result);
}

static int decode_matrix(void) {
    static const uint8_t operations[] = {0xa3, 0xab, 0xb3, 0xbb};
    static const uint8_t prefixes[][2] = {{0x66, 0}, {0, 0}, {0x48, 0}};
    uint32_t host[512]; hl_x86_a64_result result;
    for (size_t w = 0; w < 3; ++w) for (size_t op = 0; op < 4; ++op) {
        uint8_t reg[4]; size_t n = 0;
        if (prefixes[w][0]) reg[n++] = prefixes[w][0];
        reg[n++] = 0x0f; reg[n++] = operations[op]; reg[n++] = 0xc8;
        CHECK(emit(reg, n, host, &result) == HL_X86_A64_OK && result.instruction_count == 1);
        reg[n - 1] = 0x08;
        CHECK(emit(reg, n, host, &result) == HL_X86_A64_OK && result.instruction_count == 1);
        uint8_t imm[6]; size_t m = 0;
        if (prefixes[w][0]) imm[m++] = prefixes[w][0];
        imm[m++] = 0x0f; imm[m++] = 0xba; imm[m++] = (uint8_t)(0xc0 | (4 + op) * 8); imm[m++] = 0xff;
        CHECK(emit(imm, m, host, &result) == HL_X86_A64_OK && result.instruction_count == 1);
        imm[m - 2] &= 0x3f;
        CHECK(emit(imm, m, host, &result) == HL_X86_A64_OK);
    }
    { const uint8_t bad[] = {0x0f, 0xba, 0xc0, 1}; CHECK(emit(bad, sizeof bad, host, &result) == HL_X86_A64_UNSUPPORTED); }
    { const uint8_t lock[] = {0xf0, 0x0f, 0xab, 0x08}; CHECK(emit(lock, sizeof lock, host, &result) == HL_X86_A64_UNSUPPORTED); }
    { const uint8_t short_form[] = {0x0f, 0xba, 0xe0}; CHECK(emit(short_form, sizeof short_form, host, &result) == HL_X86_A64_TRUNCATED); }
    return 0;
}

static int execute_one(const uint8_t *guest, size_t size, hl_native_x86_64_cpu *cpu) {
#if !defined(__aarch64__)
    (void)guest; (void)size; (void)cpu; return 0;
#else
    long page = sysconf(_SC_PAGESIZE); uint32_t host[512]; hl_x86_a64_result result;
    uint8_t *code = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(code != MAP_FAILED); CHECK(emit(guest, size, host, &result) == HL_X86_A64_OK);
    memcpy(code, host, result.word_count * 4u); ((uint32_t *)code)[result.word_count] = UINT32_C(0xd65f03c0);
    __builtin___clear_cache((char *)code, (char *)code + (result.word_count + 1u) * 4u);
    CHECK(mprotect(code, (size_t)page, PROT_READ | PROT_EXEC) == 0); hl_x86_test_enter(cpu, code);
    CHECK(munmap(code, (size_t)page) == 0); return 0;
#endif
}

static int runtime_matrix(void) {
#if defined(__aarch64__)
    static const uint8_t bts16[] = {0x66,0x0f,0xab,0xc8};
    static const uint8_t btr32[] = {0x0f,0xb3,0xc8};
    static const uint8_t btc64[] = {0x48,0x0f,0xbb,0xc8};
    hl_native_x86_64_cpu cpu = {0}; cpu.flags = UINT64_C(0xad6); cpu.registers[0] = UINT64_C(0xaaaa555500000000); cpu.registers[1] = 15;
    CHECK(execute_one(bts16,sizeof bts16,&cpu)==0); CHECK(cpu.registers[0] == UINT64_C(0xaaaa555500008000)); CHECK(cpu.flags == UINT64_C(0xad6));
    cpu.registers[0]=UINT64_C(0xffff000080000000); cpu.registers[1]=31; CHECK(execute_one(btr32,sizeof btr32,&cpu)==0); CHECK(cpu.registers[0]==0); CHECK((cpu.flags&1)==1 && (cpu.flags&~1)==(UINT64_C(0xad6)&~1));
    cpu.registers[0]=1; cpu.registers[1]=64; CHECK(execute_one(btc64,sizeof btc64,&cpu)==0); CHECK(cpu.registers[0]==0 && (cpu.flags&1)==1);
    { /* Source/destination alias must capture the index before mutation. */
        static const uint8_t alias[] = {0x48,0x0f,0xbb,0xc0};
        cpu.registers[0]=1; cpu.flags=0xad6; CHECK(execute_one(alias,sizeof alias,&cpu)==0);
        CHECK(cpu.registers[0]==3 && cpu.flags==0xad6);
    }
    {
        static const uint8_t bts_mem[] = {0x48,0x0f,0xab,0x08}; uint8_t data[32]={0};
        memset(&cpu,0,sizeof cpu); cpu.registers[0]=0x1008; cpu.registers[1]=UINT64_MAX; cpu.flags=0xad7;
        cpu.dirty_first=UINT64_MAX;
        cpu.memory_first=0x1000; cpu.memory_last=0x1020; cpu.memory_delta=(uint64_t)(uintptr_t)data-0x1000; cpu.memory_permissions=3;
        CHECK(execute_one(bts_mem,sizeof bts_mem,&cpu)==0); CHECK(data[7]==0x80); CHECK(cpu.dirty_first==0x1007 && cpu.dirty_last==0x1008); CHECK(cpu.flags==0xad6);
        data[7]=0; cpu.flags=0xad7; cpu.memory_permissions=1; cpu.dirty_first=UINT64_MAX; cpu.dirty_last=0;
        CHECK(execute_one(bts_mem,sizeof bts_mem,&cpu)==0); CHECK(data[7]==0 && cpu.flags==0xad7 && cpu.dirty_first==UINT64_MAX && cpu.dirty_last==0 && cpu.fault_address==0x1007 && cpu.fault_size==1);
    }
    {
        static const uint8_t bt_mem[] = {0x0f,0xa3,0x08};
        static const uint8_t btc_imm[] = {0x48,0x0f,0xba,0x38,0xff};
        uint8_t data[32]={0}; memset(&cpu,0,sizeof cpu); data[7]=0x80;
        cpu.registers[0]=0x1000; cpu.registers[1]=63; cpu.flags=0xad6; cpu.dirty_first=UINT64_MAX;
        cpu.memory_first=0x1000; cpu.memory_last=0x1020; cpu.memory_delta=(uint64_t)(uintptr_t)data-0x1000; cpu.memory_permissions=1;
        CHECK(execute_one(bt_mem,sizeof bt_mem,&cpu)==0); CHECK(cpu.flags==0xad7 && data[7]==0x80); CHECK(cpu.dirty_first==UINT64_MAX && cpu.memory_written==0);
        cpu.flags=0xad6; cpu.memory_permissions=7; CHECK(execute_one(btc_imm,sizeof btc_imm,&cpu)==0);
        CHECK(data[7]==0 && cpu.flags==0xad7 && cpu.dirty_first==0x1007 && cpu.dirty_last==0x1008 && cpu.executable_written==7);
    }
#endif
    return 0;
}

int main(void) { int status=decode_matrix(); return status != 0 ? status : runtime_matrix(); }
