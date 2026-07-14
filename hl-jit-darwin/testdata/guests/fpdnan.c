// Default/indefinite-NaN sign differential (the DIVSS/DIVPS/DIVSD/DIVPD follow-up to H10-H13).
// x86 GENERATES the QNaN floating-point INDEFINITE (sign bit SET: single 0xFFC00000, double
// 0xFFF8000000000000) whenever an FP op produces a NaN with no NaN input (0/0, inf/inf, 0*inf,
// inf-inf, sqrt(<0)). ARM's hardware FDIV/FMUL/... produce the DEFAULT NaN with sign CLEAR
// (0x7FC00000 / 0x7FF8000000000000). The hl lowering now stamps x86's sign on GENERATED NaNs only --
// a NaN PROPAGATED from an input keeps that input's sign on both ISAs (critical: 2.0*QNaN(+) must stay
// positive, i.e. NOT be over-flipped). Every operand goes through a volatile sink so -O2 cannot
// constant-fold; the real SSE instruction runs and we print raw bit patterns -> byte-exact vs qemu.
#include <emmintrin.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static volatile float g_f;
static volatile double g_d;
static float VF(float x) { g_f = x; return g_f; }
static double VD(double x) { g_d = x; return g_d; }
static uint32_t bf(float x) { uint32_t u; memcpy(&u, &x, 4); return u; }
static uint64_t bd(double x) { uint64_t u; memcpy(&u, &x, 8); return u; }
static float bits_f(uint32_t u) { volatile uint32_t v = u; uint32_t w = v; float f; memcpy(&f, &w, 4); return f; }
static double bits_d(uint64_t u) { volatile uint64_t v = u; uint64_t w = v; double f; memcpy(&f, &w, 8); return f; }

static void scalar_single(void) {
    float pinf = bits_f(0x7f800000u), ninf = bits_f(0xff800000u);
    float qnp = bits_f(0x7fc00000u), qnn = bits_f(0xffc00000u); // +QNaN / -QNaN
    float zero = VF(0.0f), two = VF(2.0f), four = VF(4.0f), negone = VF(-1.0f);
    // ---- GENERATED default NaN: expect x86 negative indefinite 0xFFC00000 ----
    printf("divss 0/0    %08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(zero), _mm_set_ss(zero)))));
    printf("divss inf/inf%08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(pinf), _mm_set_ss(pinf)))));
    printf("mulss 0*inf  %08x\n", bf(_mm_cvtss_f32(_mm_mul_ss(_mm_set_ss(zero), _mm_set_ss(pinf)))));
    printf("subss inf-inf%08x\n", bf(_mm_cvtss_f32(_mm_sub_ss(_mm_set_ss(pinf), _mm_set_ss(pinf)))));
    printf("addss inf+nin%08x\n", bf(_mm_cvtss_f32(_mm_add_ss(_mm_set_ss(pinf), _mm_set_ss(ninf)))));
    printf("sqrtss -1    %08x\n", bf(_mm_cvtss_f32(_mm_sqrt_ss(_mm_set_ss(negone)))));
    // ---- PROPAGATED NaN: input NaN kept verbatim (sign preserved, NOT touched) ----
    printf("divss qn+/2  %08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(qnp), _mm_set_ss(two)))));
    printf("divss 2/qn+  %08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(two), _mm_set_ss(qnp)))));
    printf("divss qn-/2  %08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(qnn), _mm_set_ss(two)))));
    printf("mulss 2*qn+  %08x\n", bf(_mm_cvtss_f32(_mm_mul_ss(_mm_set_ss(two), _mm_set_ss(qnp))))); // must stay +
    printf("addss qn-+2  %08x\n", bf(_mm_cvtss_f32(_mm_add_ss(_mm_set_ss(qnn), _mm_set_ss(two)))));
    // ---- ordinary finite results unaffected ----
    printf("divss 2/4    %08x\n", bf(_mm_cvtss_f32(_mm_div_ss(_mm_set_ss(two), _mm_set_ss(four)))));
    printf("sqrtss 4     %08x\n", bf(_mm_cvtss_f32(_mm_sqrt_ss(_mm_set_ss(four)))));
}

static void scalar_double(void) {
    double pinf = bits_d(0x7ff0000000000000ull);
    double qnp = bits_d(0x7ff8000000000000ull), qnn = bits_d(0xfff8000000000000ull);
    double zero = VD(0.0), two = VD(2.0), four = VD(4.0), negone = VD(-1.0);
    printf("divsd 0/0    %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_div_sd(_mm_set_sd(zero), _mm_set_sd(zero)))));
    printf("divsd inf/inf%016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_div_sd(_mm_set_sd(pinf), _mm_set_sd(pinf)))));
    printf("mulsd 0*inf  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_mul_sd(_mm_set_sd(zero), _mm_set_sd(pinf)))));
    printf("subsd inf-inf%016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_sub_sd(_mm_set_sd(pinf), _mm_set_sd(pinf)))));
    printf("sqrtsd -1    %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_sqrt_sd(_mm_set_sd(zero), _mm_set_sd(negone)))));
    printf("divsd qn+/2  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_div_sd(_mm_set_sd(qnp), _mm_set_sd(two)))));
    printf("divsd qn-/2  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_div_sd(_mm_set_sd(qnn), _mm_set_sd(two)))));
    printf("mulsd 2*qn+  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_mul_sd(_mm_set_sd(two), _mm_set_sd(qnp)))));
    printf("divsd 2/4    %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_div_sd(_mm_set_sd(two), _mm_set_sd(four)))));
}

static void packed(void) {
    float pinf = bits_f(0x7f800000u), qnp = bits_f(0x7fc00000u);
    // lane0 0/0 (generated -> 0xFFC00000), lane1 qn+/2 (propagated +), lane2 2/4 (finite), lane3 inf/inf (generated)
    __m128 a = _mm_set_ps(pinf, VF(2.0f), qnp, VF(0.0f));
    __m128 b = _mm_set_ps(pinf, VF(4.0f), VF(2.0f), VF(0.0f));
    __m128 r = _mm_div_ps(a, b);
    float f[4]; _mm_storeu_ps(f, r);
    printf("divps        %08x %08x %08x %08x\n", bf(f[0]), bf(f[1]), bf(f[2]), bf(f[3]));
    __m128 sq = _mm_sqrt_ps(_mm_set_ps(VF(-1.0f), VF(4.0f), VF(-0.0f), VF(9.0f)));
    _mm_storeu_ps(f, sq);
    printf("sqrtps       %08x %08x %08x %08x\n", bf(f[0]), bf(f[1]), bf(f[2]), bf(f[3]));
    double pinfd = bits_d(0x7ff0000000000000ull), qnpd = bits_d(0x7ff8000000000000ull);
    __m128d ad = _mm_set_pd(qnpd, VD(0.0)), bd_ = _mm_set_pd(VD(2.0), VD(0.0)); // lane0 0/0 gen, lane1 qn+/2 prop
    __m128d rd = _mm_div_pd(ad, bd_);
    double dd[2]; _mm_storeu_pd(dd, rd);
    printf("divpd        %016llx %016llx\n", (unsigned long long)bd(dd[0]), (unsigned long long)bd(dd[1]));
    (void)pinfd;
}

int main(void) {
    scalar_single();
    scalar_double();
    packed();
    return 0;
}
