// Tail-call chains through a 128-entry function-pointer ring. Each function computes, then tail-calls
// the next via an indirect call whose target is data-dependent -- the sibling-call / trampoline shape
// of CPS-style code. Under -O2 these become tail jumps (block-chaining + indirect-branch prediction).
// Bounded by a step counter carried in the argument. Deterministic checksum.
#include <stdint.h>
#include <stdio.h>

#define R 128
typedef uint64_t (*link_t)(uint64_t acc, uint64_t steps);
static link_t ring[R];

static uint64_t l0(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(0+1)) ^ (acc >> (0&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l1(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(1+1)) ^ (acc >> (1&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l2(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(2+1)) ^ (acc >> (2&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l3(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(3+1)) ^ (acc >> (3&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l4(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(4+1)) ^ (acc >> (4&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l5(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(5+1)) ^ (acc >> (5&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l6(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(6+1)) ^ (acc >> (6&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l7(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(7+1)) ^ (acc >> (7&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l8(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(8+1)) ^ (acc >> (8&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l9(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(9+1)) ^ (acc >> (9&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l10(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(10+1)) ^ (acc >> (10&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l11(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(11+1)) ^ (acc >> (11&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l12(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(12+1)) ^ (acc >> (12&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l13(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(13+1)) ^ (acc >> (13&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l14(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(14+1)) ^ (acc >> (14&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l15(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(15+1)) ^ (acc >> (15&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l16(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(16+1)) ^ (acc >> (16&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l17(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(17+1)) ^ (acc >> (17&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l18(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(18+1)) ^ (acc >> (18&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l19(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(19+1)) ^ (acc >> (19&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l20(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(20+1)) ^ (acc >> (20&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l21(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(21+1)) ^ (acc >> (21&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l22(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(22+1)) ^ (acc >> (22&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l23(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(23+1)) ^ (acc >> (23&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l24(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(24+1)) ^ (acc >> (24&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l25(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(25+1)) ^ (acc >> (25&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l26(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(26+1)) ^ (acc >> (26&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l27(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(27+1)) ^ (acc >> (27&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l28(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(28+1)) ^ (acc >> (28&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l29(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(29+1)) ^ (acc >> (29&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l30(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(30+1)) ^ (acc >> (30&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l31(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(31+1)) ^ (acc >> (31&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l32(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(32+1)) ^ (acc >> (32&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l33(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(33+1)) ^ (acc >> (33&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l34(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(34+1)) ^ (acc >> (34&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l35(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(35+1)) ^ (acc >> (35&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l36(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(36+1)) ^ (acc >> (36&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l37(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(37+1)) ^ (acc >> (37&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l38(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(38+1)) ^ (acc >> (38&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l39(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(39+1)) ^ (acc >> (39&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l40(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(40+1)) ^ (acc >> (40&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l41(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(41+1)) ^ (acc >> (41&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l42(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(42+1)) ^ (acc >> (42&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l43(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(43+1)) ^ (acc >> (43&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l44(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(44+1)) ^ (acc >> (44&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l45(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(45+1)) ^ (acc >> (45&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l46(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(46+1)) ^ (acc >> (46&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l47(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(47+1)) ^ (acc >> (47&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l48(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(48+1)) ^ (acc >> (48&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l49(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(49+1)) ^ (acc >> (49&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l50(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(50+1)) ^ (acc >> (50&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l51(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(51+1)) ^ (acc >> (51&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l52(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(52+1)) ^ (acc >> (52&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l53(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(53+1)) ^ (acc >> (53&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l54(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(54+1)) ^ (acc >> (54&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l55(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(55+1)) ^ (acc >> (55&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l56(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(56+1)) ^ (acc >> (56&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l57(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(57+1)) ^ (acc >> (57&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l58(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(58+1)) ^ (acc >> (58&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l59(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(59+1)) ^ (acc >> (59&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l60(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(60+1)) ^ (acc >> (60&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l61(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(61+1)) ^ (acc >> (61&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l62(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(62+1)) ^ (acc >> (62&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l63(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(63+1)) ^ (acc >> (63&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l64(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(64+1)) ^ (acc >> (64&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l65(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(65+1)) ^ (acc >> (65&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l66(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(66+1)) ^ (acc >> (66&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l67(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(67+1)) ^ (acc >> (67&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l68(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(68+1)) ^ (acc >> (68&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l69(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(69+1)) ^ (acc >> (69&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l70(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(70+1)) ^ (acc >> (70&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l71(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(71+1)) ^ (acc >> (71&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l72(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(72+1)) ^ (acc >> (72&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l73(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(73+1)) ^ (acc >> (73&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l74(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(74+1)) ^ (acc >> (74&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l75(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(75+1)) ^ (acc >> (75&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l76(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(76+1)) ^ (acc >> (76&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l77(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(77+1)) ^ (acc >> (77&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l78(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(78+1)) ^ (acc >> (78&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l79(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(79+1)) ^ (acc >> (79&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l80(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(80+1)) ^ (acc >> (80&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l81(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(81+1)) ^ (acc >> (81&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l82(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(82+1)) ^ (acc >> (82&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l83(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(83+1)) ^ (acc >> (83&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l84(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(84+1)) ^ (acc >> (84&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l85(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(85+1)) ^ (acc >> (85&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l86(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(86+1)) ^ (acc >> (86&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l87(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(87+1)) ^ (acc >> (87&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l88(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(88+1)) ^ (acc >> (88&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l89(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(89+1)) ^ (acc >> (89&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l90(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(90+1)) ^ (acc >> (90&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l91(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(91+1)) ^ (acc >> (91&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l92(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(92+1)) ^ (acc >> (92&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l93(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(93+1)) ^ (acc >> (93&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l94(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(94+1)) ^ (acc >> (94&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l95(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(95+1)) ^ (acc >> (95&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l96(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(96+1)) ^ (acc >> (96&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l97(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(97+1)) ^ (acc >> (97&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l98(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(98+1)) ^ (acc >> (98&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l99(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(99+1)) ^ (acc >> (99&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l100(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(100+1)) ^ (acc >> (100&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l101(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(101+1)) ^ (acc >> (101&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l102(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(102+1)) ^ (acc >> (102&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l103(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(103+1)) ^ (acc >> (103&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l104(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(104+1)) ^ (acc >> (104&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l105(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(105+1)) ^ (acc >> (105&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l106(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(106+1)) ^ (acc >> (106&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l107(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(107+1)) ^ (acc >> (107&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l108(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(108+1)) ^ (acc >> (108&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l109(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(109+1)) ^ (acc >> (109&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l110(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(110+1)) ^ (acc >> (110&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l111(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(111+1)) ^ (acc >> (111&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l112(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(112+1)) ^ (acc >> (112&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l113(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(113+1)) ^ (acc >> (113&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l114(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(114+1)) ^ (acc >> (114&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l115(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(115+1)) ^ (acc >> (115&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l116(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(116+1)) ^ (acc >> (116&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l117(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(117+1)) ^ (acc >> (117&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l118(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(118+1)) ^ (acc >> (118&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l119(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(119+1)) ^ (acc >> (119&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l120(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(120+1)) ^ (acc >> (120&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l121(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(121+1)) ^ (acc >> (121&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l122(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(122+1)) ^ (acc >> (122&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l123(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(123+1)) ^ (acc >> (123&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l124(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(124+1)) ^ (acc >> (124&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l125(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(125+1)) ^ (acc >> (125&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l126(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(126+1)) ^ (acc >> (126&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }
static uint64_t l127(uint64_t acc, uint64_t steps){ acc = (acc + 0x9e3779b97f4a7c15ULL*(127+1)) ^ (acc >> (127&31)); if (steps==0) return acc; unsigned nx=(unsigned)((acc^steps)%R); return ring[nx](acc, steps-1); }

static link_t init_ring[R] = {
l0,l1,l2,l3,l4,l5,l6,l7,l8,l9,l10,l11,l12,l13,l14,l15,l16,l17,l18,l19,l20,l21,l22,l23,l24,l25,l26,l27,l28,l29,l30,l31,l32,l33,l34,l35,l36,l37,l38,l39,l40,l41,l42,l43,l44,l45,l46,l47,l48,l49,l50,l51,l52,l53,l54,l55,l56,l57,l58,l59,l60,l61,l62,l63,l64,l65,l66,l67,l68,l69,l70,l71,l72,l73,l74,l75,l76,l77,l78,l79,l80,l81,l82,l83,l84,l85,l86,l87,l88,l89,l90,l91,l92,l93,l94,l95,l96,l97,l98,l99,l100,l101,l102,l103,l104,l105,l106,l107,l108,l109,l110,l111,l112,l113,l114,l115,l116,l117,l118,l119,l120,l121,l122,l123,l124,l125,l126,l127
};

int main(void) {
    for (int i = 0; i < R; i++) ring[i] = init_ring[i];
    uint64_t acc = 0;
    for (int outer = 0; outer < 20000; outer++)
        acc ^= ring[(unsigned)(acc + (unsigned)outer) % R](acc + (unsigned)outer, 4000);
    printf("tailcall acc=%llu\n", (unsigned long long)acc);
    return 0;
}
