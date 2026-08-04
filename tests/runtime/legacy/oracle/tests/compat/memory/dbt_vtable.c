// C++-vtable-like virtual dispatch: many "objects" each carry a pointer to a method table; a hot loop
// invokes obj->vtbl->method(obj, ...) with the receiver's dynamic type changing every iteration. This
// is the megamorphic virtual-call pattern of RyuJIT/V8/JVM-style OO code and the primary consumer of
// the indirect-branch target cache. A misrouted virtual call corrupts the checksum. Deterministic.
#include <stdint.h>
#include <stdio.h>

struct obj;
typedef uint64_t (*method_t)(struct obj *, uint64_t);
struct vtbl { method_t op; uint64_t tag; };
struct obj { const struct vtbl *v; uint64_t state; };

#define KIND 48
static uint64_t m0(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(0+1))) + (x<<(0&15)); }
static uint64_t m1(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(1+1))) + (x<<(1&15)); }
static uint64_t m2(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(2+1))) + (x<<(2&15)); }
static uint64_t m3(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(3+1))) + (x<<(3&15)); }
static uint64_t m4(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(4+1))) + (x<<(4&15)); }
static uint64_t m5(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(5+1))) + (x<<(5&15)); }
static uint64_t m6(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(6+1))) + (x<<(6&15)); }
static uint64_t m7(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(7+1))) + (x<<(7&15)); }
static uint64_t m8(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(8+1))) + (x<<(8&15)); }
static uint64_t m9(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(9+1))) + (x<<(9&15)); }
static uint64_t m10(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(10+1))) + (x<<(10&15)); }
static uint64_t m11(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(11+1))) + (x<<(11&15)); }
static uint64_t m12(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(12+1))) + (x<<(12&15)); }
static uint64_t m13(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(13+1))) + (x<<(13&15)); }
static uint64_t m14(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(14+1))) + (x<<(14&15)); }
static uint64_t m15(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(15+1))) + (x<<(15&15)); }
static uint64_t m16(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(16+1))) + (x<<(16&15)); }
static uint64_t m17(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(17+1))) + (x<<(17&15)); }
static uint64_t m18(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(18+1))) + (x<<(18&15)); }
static uint64_t m19(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(19+1))) + (x<<(19&15)); }
static uint64_t m20(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(20+1))) + (x<<(20&15)); }
static uint64_t m21(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(21+1))) + (x<<(21&15)); }
static uint64_t m22(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(22+1))) + (x<<(22&15)); }
static uint64_t m23(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(23+1))) + (x<<(23&15)); }
static uint64_t m24(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(24+1))) + (x<<(24&15)); }
static uint64_t m25(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(25+1))) + (x<<(25&15)); }
static uint64_t m26(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(26+1))) + (x<<(26&15)); }
static uint64_t m27(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(27+1))) + (x<<(27&15)); }
static uint64_t m28(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(28+1))) + (x<<(28&15)); }
static uint64_t m29(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(29+1))) + (x<<(29&15)); }
static uint64_t m30(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(30+1))) + (x<<(30&15)); }
static uint64_t m31(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(31+1))) + (x<<(31&15)); }
static uint64_t m32(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(32+1))) + (x<<(32&15)); }
static uint64_t m33(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(33+1))) + (x<<(33&15)); }
static uint64_t m34(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(34+1))) + (x<<(34&15)); }
static uint64_t m35(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(35+1))) + (x<<(35&15)); }
static uint64_t m36(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(36+1))) + (x<<(36&15)); }
static uint64_t m37(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(37+1))) + (x<<(37&15)); }
static uint64_t m38(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(38+1))) + (x<<(38&15)); }
static uint64_t m39(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(39+1))) + (x<<(39&15)); }
static uint64_t m40(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(40+1))) + (x<<(40&15)); }
static uint64_t m41(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(41+1))) + (x<<(41&15)); }
static uint64_t m42(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(42+1))) + (x<<(42&15)); }
static uint64_t m43(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(43+1))) + (x<<(43&15)); }
static uint64_t m44(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(44+1))) + (x<<(44&15)); }
static uint64_t m45(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(45+1))) + (x<<(45&15)); }
static uint64_t m46(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(46+1))) + (x<<(46&15)); }
static uint64_t m47(struct obj *o, uint64_t x){ return (o->state ^ (0x100000001b3ULL*(47+1))) + (x<<(47&15)); }

static const struct vtbl tables[KIND] = {
{m0,0},{m1,1},{m2,2},{m3,3},{m4,4},{m5,5},{m6,6},{m7,7},{m8,8},{m9,9},{m10,10},{m11,11},{m12,12},{m13,13},{m14,14},{m15,15},{m16,16},{m17,17},{m18,18},{m19,19},{m20,20},{m21,21},{m22,22},{m23,23},{m24,24},{m25,25},{m26,26},{m27,27},{m28,28},{m29,29},{m30,30},{m31,31},{m32,32},{m33,33},{m34,34},{m35,35},{m36,36},{m37,37},{m38,38},{m39,39},{m40,40},{m41,41},{m42,42},{m43,43},{m44,44},{m45,45},{m46,46},{m47,47}
};

int main(void) {
    struct obj o = {&tables[0], 0xcbf29ce484222325ULL};
    uint64_t acc = 0;
    for (uint64_t i = 0; i < 30000000ULL; i++) {
        unsigned k = (unsigned)((o.state ^ (o.state >> 29) ^ i) % KIND);
        o.v = &tables[k];
        acc = o.v->op(&o, i);
        o.state = acc ^ o.v->tag;
    }
    printf("vtable acc=%llu\n", (unsigned long long)acc);
    return 0;
}
