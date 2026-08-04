#include "../src/arch/aarch64/entry.h"
#include "../src/arch/aarch64/pair_arithmetic.h"
#include "../include/executor.h"

#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "pair-arithmetic:%d: %s\n", __LINE__, #x); return 1; } } while (0)

static uint32_t pair(unsigned q, unsigned u, unsigned size, unsigned opcode,
                     unsigned rm, unsigned rn, unsigned rd) {
    return UINT32_C(0x0e200400) | (q << 30) | (u << 29) | (size << 22) |
           (rm << 16) | (opcode << 11) | (rn << 5) | rd;
}

static uint64_t lane(const uint8_t *source, unsigned index, unsigned bytes) {
    uint64_t value = 0;
    memcpy(&value, source + index * bytes, bytes);
    return value;
}

static void put(uint8_t *destination, unsigned index, unsigned bytes, uint64_t value) {
    memcpy(destination + index * bytes, &value, bytes);
}

static int64_t signed_lane(uint64_t value, unsigned bits) {
    uint64_t sign = UINT64_C(1) << (bits - 1u);
    return (int64_t)((value ^ sign) - sign);
}

static void expected(uint32_t word, const uint8_t before[32][16], uint8_t output[16]) {
    unsigned q = (word >> 30) & 1u, u = (word >> 29) & 1u, size = (word >> 22) & 3u;
    unsigned opcode = (word >> 11) & 31u, rm = (word >> 16) & 31u, rn = (word >> 5) & 31u;
    unsigned fp = opcode >= 0x18u, bytes = fp ? ((size & 1u) ? 8u : 4u) : 1u << size;
    unsigned lanes = (q ? 16u : 8u) / bytes, bits = bytes * 8u;
    uint64_t mask = bits == 64 ? UINT64_MAX : (UINT64_C(1) << bits) - 1u;
    memset(output, 0, 16);
    for (unsigned index = 0; index < lanes; index++) {
        const uint8_t *source = index < lanes / 2u ? before[rn] : before[rm];
        unsigned base = (index % (lanes / 2u)) * 2u;
        uint64_t a = lane(source, base, bytes), b = lane(source, base + 1u, bytes), value;
        if (!fp) {
            if (opcode == 0x17u) value = a + b;
            else if (u) value = opcode == 0x14u ? (a > b ? a : b) : (a < b ? a : b);
            else {
                int64_t x = signed_lane(a, bits), y = signed_lane(b, bits);
                value = (opcode == 0x14u ? x > y : x < y) ? a : b;
            }
        } else if (opcode == 0x1au) {
            if (bytes == 4) {
                float x, y, z;
                memcpy(&x, &a, 4); memcpy(&y, &b, 4); z = x + y; memcpy(&value, &z, 4);
            } else {
                double x, y, z;
                memcpy(&x, &a, 8); memcpy(&y, &b, 8); z = x + y; memcpy(&value, &z, 8);
            }
        } else {
            int want_max = (size & 2u) == 0;
            if (bytes == 4) {
                float x, y; memcpy(&x, &a, 4); memcpy(&y, &b, 4); value = (want_max ? x > y : x < y) ? a : b;
            } else {
                double x, y; memcpy(&x, &a, 8); memcpy(&y, &b, 8); value = (want_max ? x > y : x < y) ? a : b;
            }
        }
        put(output, index, bytes, value & mask);
    }
}

static void execute(hl_native_aarch64_cpu *cpu, void *address) {
    void (*entry)(void); memcpy(&entry, &address, sizeof(entry)); hl_native_aarch64_enter(cpu, entry);
}

static void append(uint32_t words[46], size_t *count, unsigned q, unsigned u,
                   unsigned size, unsigned opcode) {
    unsigned i = (unsigned)*count, rn = (i * 5u + 3u) & 31u, rm = (i * 9u + 7u) & 31u;
    unsigned rd = i % 3u == 0 ? rn : i % 3u == 1 ? rm : (i * 13u + 11u) & 31u;
    words[(*count)++] = pair(q, u, size, opcode, rm, rn, rd);
}

int main(void) {
#if !defined(__aarch64__)
    return 0;
#else
    uint32_t words[46]; size_t count = 0;
    for (unsigned q = 0; q <= 1; q++) for (unsigned size = 0; size < (q ? 4u : 3u); size++) append(words,&count,q,0,size,0x17);
    for (unsigned op = 0x14; op <= 0x15; op++) for (unsigned u=0;u<=1;u++) for(unsigned q=0;q<=1;q++) for(unsigned s=0;s<3;s++) append(words,&count,q,u,s,op);
    append(words,&count,0,1,0,0x1a); append(words,&count,1,1,0,0x1a); append(words,&count,1,1,1,0x1a);
    static const unsigned fpops[]={0x18,0x1e};
    for(size_t o=0;o<2;o++) for(unsigned s=0;s<4;s++) for(unsigned q=(s&1u);q<=1;q++) append(words,&count,q,1,s,fpops[o]);
    CHECK(count==46);
    hl_a64_assembler assembler; uint8_t encoded[4];
    for(size_t i=0;i<count;i++){CHECK(hl_a64_assembler_begin(&assembler,encoded,encoded,4));CHECK(hl_a64_pair_arithmetic_body(&assembler,words[i]));CHECK(!memcmp(encoded,&words[i],4));}
    const uint32_t invalid[]={pair(0,0,3,0x17,2,1,0),pair(1,1,0,0x17,2,1,0),pair(1,0,3,0x14,2,1,0),pair(1,1,3,0x15,2,1,0),pair(0,1,1,0x1a,2,1,0),pair(1,1,2,0x1a,2,1,0),pair(0,1,1,0x18,2,1,0),pair(1,0,0,0x1e,2,1,0)};
    for(size_t i=0;i<sizeof(invalid)/4;i++){CHECK(hl_a64_assembler_begin(&assembler,encoded,encoded,4));CHECK(!hl_a64_pair_arithmetic_body(&assembler,invalid[i]));CHECK(hl_a64_assembler_size(&assembler)==0);}
    uint8_t short_buffer[HL_A64_PAIR_ARITHMETIC_MAX_BYTES-1]; memset(short_buffer,0xa5,sizeof(short_buffer));
    CHECK(hl_a64_assembler_begin(&assembler,short_buffer,short_buffer,sizeof(short_buffer)));CHECK(!hl_a64_pair_arithmetic_emit(&assembler,words[0],0x4000));CHECK(hl_a64_assembler_size(&assembler)==0);
    long page=sysconf(_SC_PAGESIZE);CHECK(page>0);size_t capacity=(size_t)page*20;uint8_t *code=mmap(NULL,capacity,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);CHECK(code!=MAP_FAILED);CHECK(hl_a64_assembler_begin(&assembler,code,code,capacity));size_t offsets[46];
    for(size_t i=0;i<count;i++){offsets[i]=hl_a64_assembler_size(&assembler);CHECK(hl_a64_pair_arithmetic_emit(&assembler,words[i],0x8000+i*4));}__builtin___clear_cache((char*)code,(char*)code+hl_a64_assembler_size(&assembler));CHECK(!mprotect(code,capacity,PROT_READ|PROT_EXEC));
    uint8_t stack[256] __attribute__((aligned(16)));
    for(size_t i=0;i<count;i++){hl_native_aarch64_cpu cpu;memset(&cpu,0,sizeof(cpu));for(unsigned v=0;v<32;v++)for(unsigned b=0;b<16;b++)((uint8_t*)cpu.vectors)[v*16+b]=(uint8_t)(0x81u+v*23u+b*31u);unsigned op=(words[i]>>11)&31u;if(op>=0x18){unsigned bytes=((words[i]>>22)&1u)?8:4;uint64_t vals[4]={bytes==4?UINT32_C(0x3f800000):UINT64_C(0x3ff0000000000000),bytes==4?UINT32_C(0x40000000):UINT64_C(0x4000000000000000),bytes==4?UINT32_C(0xc0800000):UINT64_C(0xc010000000000000),bytes==4?UINT32_C(0x41000000):UINT64_C(0x4020000000000000)};unsigned rn=(words[i]>>5)&31,rm=(words[i]>>16)&31;for(unsigned l=0;l<16/bytes;l++){put((uint8_t*)&cpu.vectors[rn*2],l,bytes,vals[l&3]);put((uint8_t*)&cpu.vectors[rm*2],l,bytes,vals[(l+1)&3]);}}for(unsigned r=0;r<31;r++)cpu.registers[r]=UINT64_C(0x789a000000000000)+r;cpu.stack=(uint64_t)(uintptr_t)(stack+256);cpu.flags=UINT64_C(0x90000000);cpu.fpsr=UINT64_C(0x10);uint64_t regs[31],vecs[64];memcpy(regs,cpu.registers,sizeof(regs));memcpy(vecs,cpu.vectors,sizeof(vecs));uint8_t result[16];expected(words[i],(const uint8_t(*)[16])vecs,result);execute(&cpu,code+offsets[i]);CHECK(cpu.reason==HL_NATIVE_EXIT_BRANCH&&cpu.program==0x8004+i*4);CHECK(cpu.flags==UINT64_C(0x90000000)&&cpu.fpcr==0&&cpu.fpsr==UINT64_C(0x10));CHECK(!memcmp(cpu.registers,regs,sizeof(regs)));unsigned rd=words[i]&31;CHECK(!memcmp(&cpu.vectors[rd*2],result,16));for(unsigned v=0;v<32;v++)if(v!=rd)CHECK(!memcmp(&cpu.vectors[v*2],&vecs[v*2],16));}
    struct fp_special { size_t offset; uint64_t source; uint64_t fpcr, expected, fpsr; } specials[] = {
        {31, UINT64_C(0x0000000080000000), UINT64_C(2) << 22, UINT32_C(0x80000000), UINT64_C(0x10)},
        {31, UINT64_C(0x3f8000007f800045), 0, UINT32_C(0x7fc00045), UINT64_C(0x11)},
        {31, UINT64_C(0x3f8000007f800045), UINT64_C(1) << 25, UINT32_C(0x7fc00000), UINT64_C(0x11)},
        {34, UINT64_C(0x3f8000007fc00123), 0, UINT32_C(0x3f800000), UINT64_C(0x10)},
        {40, UINT64_C(0x3f8000007fc00123), 0, UINT32_C(0x7fc00123), UINT64_C(0x10)},
        {40, UINT64_C(0x0000000080000000), 0, 0, UINT64_C(0x10)},
        {43, UINT64_C(0x0000000080000000), 0, UINT32_C(0x80000000), UINT64_C(0x10)},
    };
    for (size_t i = 0; i < sizeof(specials) / sizeof(specials[0]); i++) {
        hl_native_aarch64_cpu cpu; memset(&cpu, 0, sizeof(cpu));
        unsigned rn = (words[specials[i].offset] >> 5) & 31u, rd = words[specials[i].offset] & 31u;
        memcpy(&cpu.vectors[rn * 2], &specials[i].source, 8);
        cpu.stack = (uint64_t)(uintptr_t)(stack + 256); cpu.flags = UINT64_C(0x90000000);
        cpu.fpcr = specials[i].fpcr; cpu.fpsr = UINT64_C(0x10);
        execute(&cpu, code + offsets[specials[i].offset]);
        uint64_t actual = cpu.vectors[rd * 2];
        CHECK((actual & UINT64_C(0xffffffff)) == specials[i].expected);
        if (cpu.vectors[rd * 2 + 1] != 0 || cpu.fpsr != specials[i].fpsr || cpu.fpcr != specials[i].fpcr) {
            fprintf(stderr, "pair special %zu upper64=%llx fpsr=%llx\n", i,
                    (unsigned long long)cpu.vectors[rd * 2 + 1], (unsigned long long)cpu.fpsr);
            return 1;
        }
    }
    CHECK(!munmap(code,capacity));return 0;
#endif
}
