// The x87 TAG WORD and the #IS stack faults it makes observable. A model with no tag state cannot report
// any of this and answers "success" instead: a stack overflow (which real code provokes on purpose to
// measure stack depth) silently destroys a register, FXAM never returns "empty", FFREE is a no-op, and
// FNSTENV/FNSAVE report a tag word of all-empty regardless of what is live.
//
// Deliberately NOT printed: the environment's FIP/FCS/FOP/FDP/FDS group (addresses, so not reproducible
// across builds) and the register area of EMPTY slots (hardware leaves the previous physical contents
// there, which depends on what ran before).
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static unsigned fsw(void) {
    unsigned short s;
    __asm__ volatile("fnstsw %0" : "=am"(s));
    return s;
}
static unsigned fcw(void) {
    unsigned short c;
    __asm__ volatile("fnstcw %0" : "=m"(c));
    return c;
}
static void setcw(unsigned short c) { __asm__ volatile("fldcw %0" : : "m"(c)); }
static void init(void) { __asm__ volatile("fninit"); }

// IE(0) SF(6) C0(8) C1(9) C2(10) TOP(13:11) C3(14)
static void sw(const char *tag) {
    unsigned s = fsw();
    printf("%-26s top=%u cc=%u%u%u ie=%u sf=%u exc=%02x\n", tag, (s >> 11) & 7, (s >> 14) & 1, (s >> 10) & 1,
           (s >> 8) & 1, s & 1, (s >> 6) & 1, s & 0x3f);
}
static void c1(const char *tag) { printf("%-26s c1=%u ie=%u sf=%u\n", tag, (fsw() >> 9) & 1, fsw() & 1, (fsw() >> 6) & 1); }

static unsigned tagword(void) {
    unsigned char env[28];
    __asm__ volatile("fnstenv %0" : "=m"(env) : : "memory");
    return *(uint16_t *)(env + 8);
}

/* ---- overflow: a 9th push onto a full stack. IE|SF|C1, TOP still moves, the value is DESTROYED. */
static void overflow(void) {
    puts("-- overflow");
    double v = 7.0, out;
    init();
    for (int i = 0; i < 8; i++) __asm__ volatile("fldl %0" : : "m"(v));
    sw("8 pushes");
    printf("8 pushes tw=%04x\n", tagword());
    init();
    for (int i = 0; i < 8; i++) __asm__ volatile("fldl %0" : : "m"(v));
    __asm__ volatile("fldl %0" : : "m"(v));
    sw("9th push");
    __asm__ volatile("fstpl %0" : "=m"(out));
    printf("9th push st0 bits=%016llx\n", (unsigned long long)*(uint64_t *)&out);

    // The depth probe: push until IE. A tag-less model never stops.
    init();
    int n = 0;
    for (; n < 16; n++) {
        __asm__ volatile("fld1");
        if (fsw() & 1) break;
    }
    printf("depth probe: ie after %d pushes\n", n + 1);
}

/* ---- underflow: reading an empty ST(i). IE|SF, C1=0, the destination takes the indefinite. */
static void underflow(void) {
    puts("-- underflow");
    double one = 1.0, o64;
    int32_t o32;
    int16_t o16;
    unsigned char m80[10];

    init();
    __asm__ volatile("fstpl %0" : "=m"(o64));
    c1("fstp m64 empty");
    printf("fstp m64 empty  = %016llx top=%u\n", (unsigned long long)*(uint64_t *)&o64, (fsw() >> 11) & 7);
    init();
    __asm__ volatile("fistpl %0" : "=m"(o32));
    c1("fistp m32 empty");
    printf("fistp m32 empty = %08x\n", (unsigned)o32);
    init();
    __asm__ volatile("fistps %0" : "=m"(o16));
    printf("fistp m16 empty = %04x\n", (unsigned short)o16);
    init();
    __asm__ volatile("fstpt %0" : "=m"(m80) : : "memory");
    printf("fstp m80 empty  = ");
    for (int i = 9; i >= 0; i--) printf("%02x", m80[i]);
    printf("\n");

    init();
    __asm__ volatile("fsqrt");
    c1("fsqrt empty");
    printf("fsqrt empty tw=%04x\n", tagword());
    init();
    __asm__ volatile("fchs");
    c1("fchs empty");
    init();
    __asm__ volatile("faddl %0" : : "m"(one));
    c1("fadd m64 to empty");
    init();
    __asm__ volatile("fld1\n\tfld %st(1)");
    sw("fld st(1) empty");
    printf("fld st(1) empty tw=%04x\n", tagword());
    init();
    __asm__ volatile("fld1\n\tfprem");
    c1("fprem st1 empty");
    init();
    __asm__ volatile("fld1\n\tfxtract");
    c1("fxtract from 1 deep");

    // FXCH: the exchange happens, then ST0 takes the indefinite -- ST1 keeps the OLD ST0 and is tagged live.
    init();
    __asm__ volatile("fld1\n\tfxch %st(1)");
    sw("fxch st(1) empty");
    __asm__ volatile("fstpt %0" : "=m"(m80) : : "memory");
    printf("  st0=");
    for (int i = 9; i >= 0; i--) printf("%02x", m80[i]);
    __asm__ volatile("fstpt %0" : "=m"(m80) : : "memory");
    printf(" st1=");
    for (int i = 9; i >= 0; i--) printf("%02x", m80[i]);
    printf("\n");

    // A compare against an empty register is UNORDERED as well as faulting.
    init();
    __asm__ volatile("fld1\n\tfcom %st(1)");
    sw("fcom st(1) empty");
    init();
    __asm__ volatile("fld1\n\tfucom %st(1)");
    sw("fucom st(1) empty");
    {
        unsigned long fl;
        init();
        __asm__ volatile("fld1\n\tfcomi %%st(1),%%st\n\tpushfq\n\tpop %0" : "=r"(fl));
        printf("fcomi st(1) empty fl=%03lx ie=%u sf=%u\n", fl & 0x8d5, fsw() & 1, (fsw() >> 6) & 1);
        init();
        __asm__ volatile("fld1\n\tfucomi %%st(1),%%st\n\tpushfq\n\tpop %0" : "=r"(fl));
        printf("fucomi st(1) empty fl=%03lx ie=%u\n", fl & 0x8d5, fsw() & 1);
    }
    // Writing an empty destination is LEGAL and fills it -- no fault.
    init();
    __asm__ volatile("fld1\n\tfst %st(3)");
    sw("fst st(3) empty dest");
    printf("fst st(3) tw=%04x\n", tagword());
    // FDECSTP never faults, even onto a full stack, and does not retag.
    init();
    for (int i = 0; i < 8; i++) __asm__ volatile("fld1");
    __asm__ volatile("fdecstp");
    sw("fdecstp onto full");
    init();
    __asm__ volatile("fld1\n\tfdecstp\n\tfld1");
    sw("fld1 into the gap");
    printf("fld1 into the gap tw=%04x\n", tagword());
}

/* ---- FXAM reports empty (C3:C2:C0 = 101), and FFREE is what creates it. */
static void classify(void) {
    puts("-- fxam / ffree");
    double v[5] = {0.0, -0.0, 1.0, 1.0 / 0.0, 0.0 / 0.0};
    const char *name[5] = {"+0", "-0", "1.0", "+inf", "nan"};
    init();
    __asm__ volatile("fxam");
    sw("fxam on empty");
    for (int i = 0; i < 5; i++) {
        char tag[40];
        init();
        __asm__ volatile("fldl %0\n\tfxam" : : "m"(v[i]));
        snprintf(tag, sizeof tag, "fxam %s", name[i]);
        sw(tag);
    }
    init();
    __asm__ volatile("fld1\n\tffree %st(0)\n\tfxam");
    sw("fxam after ffree st0");
    printf("ffree st0 tw=%04x\n", tagword());
    init();
    __asm__ volatile("fld1\n\tfld1\n\tfld1\n\tffree %st(1)");
    printf("ffree st1 of 3 tw=%04x fx_top=%u\n", tagword(), (fsw() >> 11) & 7);
    init();
    __asm__ volatile("fld1\n\tffree %st(0)\n\tfst %st(0)");
    c1("ffree st0 then fst st0");
    // FINCSTP moves TOP over an untagged slot: FXAM then reports empty.
    init();
    __asm__ volatile("fld1\n\tfincstp\n\tfxam");
    sw("fld1;fincstp;fxam");
}

/* ---- FNSTENV / FLDENV: the tag word round-trips, and FNSTENV masks every exception. */
static void environment(void) {
    puts("-- fnstenv / fldenv");
    unsigned char env[28], env2[28];
    double a = 1.5, b = 2.5;
    uint32_t w[7];

    init();
    printf("empty tw=%04x\n", tagword());
    init();
    __asm__ volatile("fldl %0\n\tfldl %1" : : "m"(a), "m"(b));
    memset(env, 0xAA, sizeof env);
    __asm__ volatile("fnstenv %0" : "=m"(env) : : "memory");
    memcpy(w, env, 28);
    printf("2 live: cw=%08x sw=%08x tw=%08x\n", w[0], w[1], w[2]);
    // The reserved upper halves are 0xffff-filled, not the caller's 0xAA bytes.
    printf("upper halves: %04x %04x %04x %04x\n", (unsigned)(w[0] >> 16), (unsigned)(w[1] >> 16),
           (unsigned)(w[2] >> 16), (unsigned)(w[6] >> 16));

    // Every tag class: zero(01), special(10) for inf/nan, valid(00).
    {
        double z = 0.0, inf = 1.0 / 0.0, nan = 0.0 / 0.0, one = 1.0;
        init();
        __asm__ volatile("fldl %0\n\tfldl %1\n\tfldl %2\n\tfldl %3" : : "m"(nan), "m"(inf), "m"(one), "m"(z));
        printf("nan,inf,1,0 pushed: tw=%04x top=%u\n", tagword(), (fsw() >> 11) & 7);
    }

    // FNSTENV masks all six exceptions afterwards.
    init();
    setcw(0x0300);
    printf("fcw before fnstenv=%04x\n", fcw());
    __asm__ volatile("fnstenv %0" : "=m"(env) : : "memory");
    printf("fcw after  fnstenv=%04x\n", fcw());

    // FLDENV restores TOP and the tags WITHOUT touching the data registers, so a register re-tagged live
    // reads back its pre-FNINIT value.
    init();
    __asm__ volatile("fldl %0\n\tfldl %1" : : "m"(a), "m"(b));
    __asm__ volatile("fnstenv %0" : "=m"(env2) : : "memory");
    init();
    printf("after fninit tw=%04x\n", tagword());
    __asm__ volatile("fldenv %0" : : "m"(env2) : "memory");
    printf("after fldenv tw=%04x top=%u\n", tagword(), (fsw() >> 11) & 7);
    {
        double o;
        __asm__ volatile("fstpl %0" : "=m"(o));
        printf("fldenv st0=%g ie=%u\n", o, fsw() & 1);
    }
    // Forcing the tag word to all-empty makes the same registers unreadable again.
    init();
    __asm__ volatile("fldl %0" : : "m"(a));
    __asm__ volatile("fnstenv %0" : "=m"(env2) : : "memory");
    *(uint16_t *)(env2 + 8) = 0xffff;
    init();
    __asm__ volatile("fldenv %0" : : "m"(env2) : "memory");
    __asm__ volatile("fxam");
    sw("fldenv all-empty tw");

    // FCW is not stored verbatim: bit 6 reads back 1, bit 7 and 15:13 read back 0.
    {
        unsigned short probe[4] = {0x0000, 0x0b00, 0xffff, 0x1234};
        for (int i = 0; i < 4; i++) {
            init();
            setcw(probe[i]);
            printf("fldcw %04x -> fnstcw %04x\n", probe[i], fcw());
        }
    }
}

/* ---- FNSAVE / FRSTOR m108: env + eight ext80 in ST(i) order, then a full FNINIT. */
static void savearea(void) {
    puts("-- fnsave / frstor");
    unsigned char area[108];
    double a = 1.5, b = -2.25;
    init();
    setcw(0x0a7f); // RC=up, PC=53 -- must survive the round trip
    __asm__ volatile("fldl %0\n\tfldl %1" : : "m"(a), "m"(b));
    memset(area, 0xAA, sizeof area);
    __asm__ volatile("fnsave %0" : "=m"(area) : : "memory");
    printf("saved cw=%04x sw=%04x tw=%04x\n", (unsigned)*(uint16_t *)area, (unsigned)*(uint16_t *)(area + 4),
           (unsigned)*(uint16_t *)(area + 8));
    for (int i = 0; i < 2; i++) { // only the two LIVE slots: empty ones hold stale physical bytes
        printf("saved st(%d)=", i);
        for (int j = 9; j >= 0; j--) printf("%02x", area[28 + 10 * i + j]);
        printf("\n");
    }
    printf("after fnsave: cw=%04x tw=%04x top=%u ie=%u\n", fcw(), tagword(), (fsw() >> 11) & 7, fsw() & 1);
    __asm__ volatile("frstor %0" : : "m"(area) : "memory");
    printf("after frstor: cw=%04x tw=%04x top=%u\n", fcw(), tagword(), (fsw() >> 11) & 7);
    {
        double o0, o1;
        __asm__ volatile("fstpl %0" : "=m"(o0));
        __asm__ volatile("fstpl %0" : "=m"(o1));
        printf("frstor st0=%g st1=%g ie=%u sf=%u\n", o0, o1, fsw() & 1, (fsw() >> 6) & 1);
    }
    // A hole punched by FFREE survives the round trip.
    init();
    __asm__ volatile("fld1\n\tfld1\n\tfld1\n\tffree %st(1)");
    __asm__ volatile("fnsave %0" : "=m"(area) : : "memory");
    printf("hole saved tw=%04x\n", (unsigned)*(uint16_t *)(area + 8));
    __asm__ volatile("frstor %0" : : "m"(area) : "memory");
    printf("hole restored tw=%04x\n", tagword());
    __asm__ volatile("fld1"); // TOP-1 is live -> overflow
    printf("push after restore: ie=%u c1=%u\n", fsw() & 1, (fsw() >> 9) & 1);
}

/* ---- FXSAVE: the abridged tag byte is PHYSICAL, the register area is TOP-RELATIVE. */
static void fxsave_area(void) {
    puts("-- fxsave");
    _Alignas(64) unsigned char area[512];
    double v[3] = {1.0, 2.0, 3.0};
    init();
    memset(area, 0xAA, sizeof area);
    __asm__ volatile("fldl %0\n\tfldl %1\n\tfldl %2" : : "m"(v[0]), "m"(v[1]), "m"(v[2]));
    __asm__ volatile("fxsave %0" : "=m"(area) : : "memory");
    printf("top=%u ftw_byte=%02x\n", (unsigned)((*(uint16_t *)(area + 2) >> 11) & 7), area[4]);
    for (int i = 0; i < 3; i++) { // slot i must be ST(i), not physical st[i]
        printf("fxsave slot %d=", i);
        for (int j = 9; j >= 0; j--) printf("%02x", area[32 + 16 * i + j]);
        printf("\n");
    }
    init();
    memset(area, 0xAA, sizeof area);
    __asm__ volatile("fxsave %0" : "=m"(area) : : "memory");
    printf("empty ftw_byte=%02x\n", area[4]);
    init();
    __asm__ volatile("fldl %0\n\tfldl %1\n\tfldl %2\n\tffree %%st(1)" : : "m"(v[0]), "m"(v[1]), "m"(v[2]));
    __asm__ volatile("fxsave %0" : "=m"(area) : : "memory");
    printf("hole ftw_byte=%02x\n", area[4]);
    // fxrstor must bring the tags (and therefore the faults) back.
    init();
    __asm__ volatile("fxrstor %0" : : "m"(area) : "memory");
    printf("fxrstor tw=%04x top=%u\n", tagword(), (fsw() >> 11) & 7);
    {
        double o;
        __asm__ volatile("fstpl %0" : "=m"(o));
        printf("fxrstor st0=%g ie=%u\n", o, fsw() & 1);
    }
}

int main(void) {
    overflow();
    underflow();
    classify();
    environment();
    savearea();
    fxsave_area();
    return 0;
}
