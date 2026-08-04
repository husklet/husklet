// Megamorphic indirect-branch dispatch: a 200-entry function-pointer table is called in a
// data-dependent, ever-changing order for many iterations. One wrong indirect-branch target-cache
// (IBTC) prediction routes to the wrong function and corrupts the running checksum, so a stale/
// poisoned IBTC entry surfaces as an engine-vs-golden divergence. Deterministic checksum only.
#include <stdint.h>
#include <stdio.h>

#define N 200
typedef uint64_t (*fn_t)(uint64_t, uint64_t);

static uint64_t f0(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(0+1)) ^ (i<<(0&15)); }
static uint64_t f1(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(1+1)) ^ (i<<(1&15)); }
static uint64_t f2(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(2+1)) ^ (i<<(2&15)); }
static uint64_t f3(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(3+1)) ^ (i<<(3&15)); }
static uint64_t f4(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(4+1)) ^ (i<<(4&15)); }
static uint64_t f5(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(5+1)) ^ (i<<(5&15)); }
static uint64_t f6(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(6+1)) ^ (i<<(6&15)); }
static uint64_t f7(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(7+1)) ^ (i<<(7&15)); }
static uint64_t f8(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(8+1)) ^ (i<<(8&15)); }
static uint64_t f9(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(9+1)) ^ (i<<(9&15)); }
static uint64_t f10(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(10+1)) ^ (i<<(10&15)); }
static uint64_t f11(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(11+1)) ^ (i<<(11&15)); }
static uint64_t f12(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(12+1)) ^ (i<<(12&15)); }
static uint64_t f13(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(13+1)) ^ (i<<(13&15)); }
static uint64_t f14(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(14+1)) ^ (i<<(14&15)); }
static uint64_t f15(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(15+1)) ^ (i<<(15&15)); }
static uint64_t f16(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(16+1)) ^ (i<<(16&15)); }
static uint64_t f17(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(17+1)) ^ (i<<(17&15)); }
static uint64_t f18(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(18+1)) ^ (i<<(18&15)); }
static uint64_t f19(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(19+1)) ^ (i<<(19&15)); }
static uint64_t f20(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(20+1)) ^ (i<<(20&15)); }
static uint64_t f21(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(21+1)) ^ (i<<(21&15)); }
static uint64_t f22(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(22+1)) ^ (i<<(22&15)); }
static uint64_t f23(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(23+1)) ^ (i<<(23&15)); }
static uint64_t f24(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(24+1)) ^ (i<<(24&15)); }
static uint64_t f25(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(25+1)) ^ (i<<(25&15)); }
static uint64_t f26(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(26+1)) ^ (i<<(26&15)); }
static uint64_t f27(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(27+1)) ^ (i<<(27&15)); }
static uint64_t f28(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(28+1)) ^ (i<<(28&15)); }
static uint64_t f29(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(29+1)) ^ (i<<(29&15)); }
static uint64_t f30(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(30+1)) ^ (i<<(30&15)); }
static uint64_t f31(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(31+1)) ^ (i<<(31&15)); }
static uint64_t f32(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(32+1)) ^ (i<<(32&15)); }
static uint64_t f33(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(33+1)) ^ (i<<(33&15)); }
static uint64_t f34(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(34+1)) ^ (i<<(34&15)); }
static uint64_t f35(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(35+1)) ^ (i<<(35&15)); }
static uint64_t f36(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(36+1)) ^ (i<<(36&15)); }
static uint64_t f37(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(37+1)) ^ (i<<(37&15)); }
static uint64_t f38(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(38+1)) ^ (i<<(38&15)); }
static uint64_t f39(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(39+1)) ^ (i<<(39&15)); }
static uint64_t f40(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(40+1)) ^ (i<<(40&15)); }
static uint64_t f41(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(41+1)) ^ (i<<(41&15)); }
static uint64_t f42(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(42+1)) ^ (i<<(42&15)); }
static uint64_t f43(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(43+1)) ^ (i<<(43&15)); }
static uint64_t f44(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(44+1)) ^ (i<<(44&15)); }
static uint64_t f45(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(45+1)) ^ (i<<(45&15)); }
static uint64_t f46(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(46+1)) ^ (i<<(46&15)); }
static uint64_t f47(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(47+1)) ^ (i<<(47&15)); }
static uint64_t f48(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(48+1)) ^ (i<<(48&15)); }
static uint64_t f49(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(49+1)) ^ (i<<(49&15)); }
static uint64_t f50(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(50+1)) ^ (i<<(50&15)); }
static uint64_t f51(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(51+1)) ^ (i<<(51&15)); }
static uint64_t f52(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(52+1)) ^ (i<<(52&15)); }
static uint64_t f53(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(53+1)) ^ (i<<(53&15)); }
static uint64_t f54(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(54+1)) ^ (i<<(54&15)); }
static uint64_t f55(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(55+1)) ^ (i<<(55&15)); }
static uint64_t f56(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(56+1)) ^ (i<<(56&15)); }
static uint64_t f57(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(57+1)) ^ (i<<(57&15)); }
static uint64_t f58(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(58+1)) ^ (i<<(58&15)); }
static uint64_t f59(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(59+1)) ^ (i<<(59&15)); }
static uint64_t f60(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(60+1)) ^ (i<<(60&15)); }
static uint64_t f61(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(61+1)) ^ (i<<(61&15)); }
static uint64_t f62(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(62+1)) ^ (i<<(62&15)); }
static uint64_t f63(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(63+1)) ^ (i<<(63&15)); }
static uint64_t f64(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(64+1)) ^ (i<<(64&15)); }
static uint64_t f65(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(65+1)) ^ (i<<(65&15)); }
static uint64_t f66(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(66+1)) ^ (i<<(66&15)); }
static uint64_t f67(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(67+1)) ^ (i<<(67&15)); }
static uint64_t f68(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(68+1)) ^ (i<<(68&15)); }
static uint64_t f69(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(69+1)) ^ (i<<(69&15)); }
static uint64_t f70(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(70+1)) ^ (i<<(70&15)); }
static uint64_t f71(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(71+1)) ^ (i<<(71&15)); }
static uint64_t f72(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(72+1)) ^ (i<<(72&15)); }
static uint64_t f73(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(73+1)) ^ (i<<(73&15)); }
static uint64_t f74(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(74+1)) ^ (i<<(74&15)); }
static uint64_t f75(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(75+1)) ^ (i<<(75&15)); }
static uint64_t f76(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(76+1)) ^ (i<<(76&15)); }
static uint64_t f77(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(77+1)) ^ (i<<(77&15)); }
static uint64_t f78(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(78+1)) ^ (i<<(78&15)); }
static uint64_t f79(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(79+1)) ^ (i<<(79&15)); }
static uint64_t f80(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(80+1)) ^ (i<<(80&15)); }
static uint64_t f81(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(81+1)) ^ (i<<(81&15)); }
static uint64_t f82(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(82+1)) ^ (i<<(82&15)); }
static uint64_t f83(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(83+1)) ^ (i<<(83&15)); }
static uint64_t f84(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(84+1)) ^ (i<<(84&15)); }
static uint64_t f85(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(85+1)) ^ (i<<(85&15)); }
static uint64_t f86(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(86+1)) ^ (i<<(86&15)); }
static uint64_t f87(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(87+1)) ^ (i<<(87&15)); }
static uint64_t f88(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(88+1)) ^ (i<<(88&15)); }
static uint64_t f89(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(89+1)) ^ (i<<(89&15)); }
static uint64_t f90(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(90+1)) ^ (i<<(90&15)); }
static uint64_t f91(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(91+1)) ^ (i<<(91&15)); }
static uint64_t f92(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(92+1)) ^ (i<<(92&15)); }
static uint64_t f93(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(93+1)) ^ (i<<(93&15)); }
static uint64_t f94(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(94+1)) ^ (i<<(94&15)); }
static uint64_t f95(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(95+1)) ^ (i<<(95&15)); }
static uint64_t f96(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(96+1)) ^ (i<<(96&15)); }
static uint64_t f97(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(97+1)) ^ (i<<(97&15)); }
static uint64_t f98(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(98+1)) ^ (i<<(98&15)); }
static uint64_t f99(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(99+1)) ^ (i<<(99&15)); }
static uint64_t f100(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(100+1)) ^ (i<<(100&15)); }
static uint64_t f101(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(101+1)) ^ (i<<(101&15)); }
static uint64_t f102(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(102+1)) ^ (i<<(102&15)); }
static uint64_t f103(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(103+1)) ^ (i<<(103&15)); }
static uint64_t f104(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(104+1)) ^ (i<<(104&15)); }
static uint64_t f105(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(105+1)) ^ (i<<(105&15)); }
static uint64_t f106(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(106+1)) ^ (i<<(106&15)); }
static uint64_t f107(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(107+1)) ^ (i<<(107&15)); }
static uint64_t f108(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(108+1)) ^ (i<<(108&15)); }
static uint64_t f109(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(109+1)) ^ (i<<(109&15)); }
static uint64_t f110(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(110+1)) ^ (i<<(110&15)); }
static uint64_t f111(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(111+1)) ^ (i<<(111&15)); }
static uint64_t f112(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(112+1)) ^ (i<<(112&15)); }
static uint64_t f113(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(113+1)) ^ (i<<(113&15)); }
static uint64_t f114(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(114+1)) ^ (i<<(114&15)); }
static uint64_t f115(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(115+1)) ^ (i<<(115&15)); }
static uint64_t f116(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(116+1)) ^ (i<<(116&15)); }
static uint64_t f117(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(117+1)) ^ (i<<(117&15)); }
static uint64_t f118(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(118+1)) ^ (i<<(118&15)); }
static uint64_t f119(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(119+1)) ^ (i<<(119&15)); }
static uint64_t f120(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(120+1)) ^ (i<<(120&15)); }
static uint64_t f121(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(121+1)) ^ (i<<(121&15)); }
static uint64_t f122(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(122+1)) ^ (i<<(122&15)); }
static uint64_t f123(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(123+1)) ^ (i<<(123&15)); }
static uint64_t f124(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(124+1)) ^ (i<<(124&15)); }
static uint64_t f125(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(125+1)) ^ (i<<(125&15)); }
static uint64_t f126(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(126+1)) ^ (i<<(126&15)); }
static uint64_t f127(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(127+1)) ^ (i<<(127&15)); }
static uint64_t f128(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(128+1)) ^ (i<<(128&15)); }
static uint64_t f129(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(129+1)) ^ (i<<(129&15)); }
static uint64_t f130(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(130+1)) ^ (i<<(130&15)); }
static uint64_t f131(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(131+1)) ^ (i<<(131&15)); }
static uint64_t f132(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(132+1)) ^ (i<<(132&15)); }
static uint64_t f133(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(133+1)) ^ (i<<(133&15)); }
static uint64_t f134(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(134+1)) ^ (i<<(134&15)); }
static uint64_t f135(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(135+1)) ^ (i<<(135&15)); }
static uint64_t f136(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(136+1)) ^ (i<<(136&15)); }
static uint64_t f137(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(137+1)) ^ (i<<(137&15)); }
static uint64_t f138(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(138+1)) ^ (i<<(138&15)); }
static uint64_t f139(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(139+1)) ^ (i<<(139&15)); }
static uint64_t f140(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(140+1)) ^ (i<<(140&15)); }
static uint64_t f141(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(141+1)) ^ (i<<(141&15)); }
static uint64_t f142(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(142+1)) ^ (i<<(142&15)); }
static uint64_t f143(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(143+1)) ^ (i<<(143&15)); }
static uint64_t f144(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(144+1)) ^ (i<<(144&15)); }
static uint64_t f145(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(145+1)) ^ (i<<(145&15)); }
static uint64_t f146(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(146+1)) ^ (i<<(146&15)); }
static uint64_t f147(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(147+1)) ^ (i<<(147&15)); }
static uint64_t f148(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(148+1)) ^ (i<<(148&15)); }
static uint64_t f149(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(149+1)) ^ (i<<(149&15)); }
static uint64_t f150(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(150+1)) ^ (i<<(150&15)); }
static uint64_t f151(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(151+1)) ^ (i<<(151&15)); }
static uint64_t f152(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(152+1)) ^ (i<<(152&15)); }
static uint64_t f153(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(153+1)) ^ (i<<(153&15)); }
static uint64_t f154(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(154+1)) ^ (i<<(154&15)); }
static uint64_t f155(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(155+1)) ^ (i<<(155&15)); }
static uint64_t f156(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(156+1)) ^ (i<<(156&15)); }
static uint64_t f157(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(157+1)) ^ (i<<(157&15)); }
static uint64_t f158(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(158+1)) ^ (i<<(158&15)); }
static uint64_t f159(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(159+1)) ^ (i<<(159&15)); }
static uint64_t f160(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(160+1)) ^ (i<<(160&15)); }
static uint64_t f161(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(161+1)) ^ (i<<(161&15)); }
static uint64_t f162(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(162+1)) ^ (i<<(162&15)); }
static uint64_t f163(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(163+1)) ^ (i<<(163&15)); }
static uint64_t f164(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(164+1)) ^ (i<<(164&15)); }
static uint64_t f165(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(165+1)) ^ (i<<(165&15)); }
static uint64_t f166(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(166+1)) ^ (i<<(166&15)); }
static uint64_t f167(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(167+1)) ^ (i<<(167&15)); }
static uint64_t f168(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(168+1)) ^ (i<<(168&15)); }
static uint64_t f169(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(169+1)) ^ (i<<(169&15)); }
static uint64_t f170(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(170+1)) ^ (i<<(170&15)); }
static uint64_t f171(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(171+1)) ^ (i<<(171&15)); }
static uint64_t f172(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(172+1)) ^ (i<<(172&15)); }
static uint64_t f173(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(173+1)) ^ (i<<(173&15)); }
static uint64_t f174(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(174+1)) ^ (i<<(174&15)); }
static uint64_t f175(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(175+1)) ^ (i<<(175&15)); }
static uint64_t f176(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(176+1)) ^ (i<<(176&15)); }
static uint64_t f177(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(177+1)) ^ (i<<(177&15)); }
static uint64_t f178(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(178+1)) ^ (i<<(178&15)); }
static uint64_t f179(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(179+1)) ^ (i<<(179&15)); }
static uint64_t f180(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(180+1)) ^ (i<<(180&15)); }
static uint64_t f181(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(181+1)) ^ (i<<(181&15)); }
static uint64_t f182(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(182+1)) ^ (i<<(182&15)); }
static uint64_t f183(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(183+1)) ^ (i<<(183&15)); }
static uint64_t f184(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(184+1)) ^ (i<<(184&15)); }
static uint64_t f185(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(185+1)) ^ (i<<(185&15)); }
static uint64_t f186(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(186+1)) ^ (i<<(186&15)); }
static uint64_t f187(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(187+1)) ^ (i<<(187&15)); }
static uint64_t f188(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(188+1)) ^ (i<<(188&15)); }
static uint64_t f189(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(189+1)) ^ (i<<(189&15)); }
static uint64_t f190(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(190+1)) ^ (i<<(190&15)); }
static uint64_t f191(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(191+1)) ^ (i<<(191&15)); }
static uint64_t f192(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(192+1)) ^ (i<<(192&15)); }
static uint64_t f193(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(193+1)) ^ (i<<(193&15)); }
static uint64_t f194(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(194+1)) ^ (i<<(194&15)); }
static uint64_t f195(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(195+1)) ^ (i<<(195&15)); }
static uint64_t f196(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(196+1)) ^ (i<<(196&15)); }
static uint64_t f197(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(197+1)) ^ (i<<(197&15)); }
static uint64_t f198(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(198+1)) ^ (i<<(198&15)); }
static uint64_t f199(uint64_t a, uint64_t i){ return (a*0x100000001b3ULL) ^ (0x9e3779b97f4a7c15ULL*(199+1)) ^ (i<<(199&15)); }

static fn_t table[N] = {
f0,f1,f2,f3,f4,f5,f6,f7,f8,f9,f10,f11,f12,f13,f14,f15,f16,f17,f18,f19,f20,f21,f22,f23,f24,f25,f26,f27,f28,f29,f30,f31,f32,f33,f34,f35,f36,f37,f38,f39,f40,f41,f42,f43,f44,f45,f46,f47,f48,f49,f50,f51,f52,f53,f54,f55,f56,f57,f58,f59,f60,f61,f62,f63,f64,f65,f66,f67,f68,f69,f70,f71,f72,f73,f74,f75,f76,f77,f78,f79,f80,f81,f82,f83,f84,f85,f86,f87,f88,f89,f90,f91,f92,f93,f94,f95,f96,f97,f98,f99,f100,f101,f102,f103,f104,f105,f106,f107,f108,f109,f110,f111,f112,f113,f114,f115,f116,f117,f118,f119,f120,f121,f122,f123,f124,f125,f126,f127,f128,f129,f130,f131,f132,f133,f134,f135,f136,f137,f138,f139,f140,f141,f142,f143,f144,f145,f146,f147,f148,f149,f150,f151,f152,f153,f154,f155,f156,f157,f158,f159,f160,f161,f162,f163,f164,f165,f166,f167,f168,f169,f170,f171,f172,f173,f174,f175,f176,f177,f178,f179,f180,f181,f182,f183,f184,f185,f186,f187,f188,f189,f190,f191,f192,f193,f194,f195,f196,f197,f198,f199
};

int main(void) {
    uint64_t a = 0xcbf29ce484222325ULL;
    for (uint64_t i = 0; i < 40000000ULL; i++) {
        uint32_t k = (uint32_t)((a ^ (a >> 23)) % N);
        a = table[k](a, i);
    }
    printf("ibtc-mega acc=%llu\n", (unsigned long long)a);
    return 0;
}
