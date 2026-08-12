// FP-codegen edge differential (H10 MIN/MAX, H12 CMPNLT/NLE, H13 float->int indefinite, ROUND MXCSR).
// Every operand is read through a volatile sink so gcc -O2 cannot constant-fold the intrinsic -- the
// actual SSE/SSE2/SSE4.1 instruction executes at runtime, so this diffs the hl x86 codegen against the
// qemu oracle byte-for-byte (results printed as raw hex bit patterns: NaN payload / sign-of-zero exact).
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <emmintrin.h>
#include <smmintrin.h> // SSE4.1 ROUND / cvt (function-level target attr below where needed)

static volatile float g_f;
static volatile double g_d;
static volatile int g_i;

static float VF(float x) { g_f = x; return g_f; }
static double VD(double x) { g_d = x; return g_d; }

static uint32_t bf(float x) { uint32_t u; memcpy(&u, &x, 4); return u; }
static uint64_t bd(double x) { uint64_t u; memcpy(&u, &x, 8); return u; }

// Build NaN/Inf from an EXPLICIT bit pattern (not 0.0/0.0): the default NaN emitted by a hardware divide
// differs in sign between x86 (0xFFC00000) and ARM (0x7FC00000), which is a divide-lowering detail, NOT a
// min/max/cvt concern -- so pin identical inputs on both platforms to isolate the codegen under test.
static float bits_f(uint32_t u) { g_i = (int)u; uint32_t v = (uint32_t)g_i; float f; memcpy(&f, &v, 4); return f; }
static double bits_d(uint64_t u) { double f; volatile uint64_t v = u; uint64_t w = v; memcpy(&f, &w, 8); return f; }
static float mk_nanf(void) { return bits_f(0x7fc00000u); }
static double mk_nand(void) { return bits_d(0x7ff8000000000000ull); }
static float infp_f(void) { return bits_f(0x7f800000u); }
static double infp_d(void) { return bits_d(0x7ff0000000000000ull); }

static __m128 loadps(float a, float b, float c, float d) { return _mm_set_ps(d, c, b, a); } // lane0=a..lane3=d
static void showps(const char *tag, __m128 v) {
    float f[4]; _mm_storeu_ps(f, v);
    printf("%s %08x %08x %08x %08x\n", tag, bf(f[0]), bf(f[1]), bf(f[2]), bf(f[3]));
}
static void showpd(const char *tag, __m128d v) {
    double f[2]; _mm_storeu_pd(f, v);
    printf("%s %016llx %016llx\n", tag, (unsigned long long)bd(f[0]), (unsigned long long)bd(f[1]));
}
static void showdq(const char *tag, __m128i v) {
    int32_t f[4]; _mm_storeu_si128((__m128i *)f, v);
    printf("%s %08x %08x %08x %08x\n", tag, (unsigned)f[0], (unsigned)f[1], (unsigned)f[2], (unsigned)f[3]);
}

static void test_minmax(void) {
    float nan = mk_nanf(); double nand_ = mk_nand();
    float pz = VF(0.0f), nz = VF(-0.0f), two = VF(2.0f), three = VF(3.0f);
    // scalar: result = (a<b)?a:b (min) / (a>b)?a:b (max); NaN/equal/+-0 -> src2 (=b), bits verbatim
    printf("minss nan,1  %08x\n", bf(_mm_cvtss_f32(_mm_min_ss(_mm_set_ss(nan), _mm_set_ss(VF(1.0f))))));
    printf("minss 1,nan  %08x\n", bf(_mm_cvtss_f32(_mm_min_ss(_mm_set_ss(VF(1.0f)), _mm_set_ss(nan)))));
    printf("maxss nan,1  %08x\n", bf(_mm_cvtss_f32(_mm_max_ss(_mm_set_ss(nan), _mm_set_ss(VF(1.0f))))));
    printf("minss +0,-0  %08x\n", bf(_mm_cvtss_f32(_mm_min_ss(_mm_set_ss(pz), _mm_set_ss(nz)))));
    printf("minss -0,+0  %08x\n", bf(_mm_cvtss_f32(_mm_min_ss(_mm_set_ss(nz), _mm_set_ss(pz)))));
    printf("maxss +0,-0  %08x\n", bf(_mm_cvtss_f32(_mm_max_ss(_mm_set_ss(pz), _mm_set_ss(nz)))));
    printf("minss 2,3    %08x\n", bf(_mm_cvtss_f32(_mm_min_ss(_mm_set_ss(two), _mm_set_ss(three)))));
    printf("maxss 2,3    %08x\n", bf(_mm_cvtss_f32(_mm_max_ss(_mm_set_ss(two), _mm_set_ss(three)))));
    printf("minsd nan,1  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_min_sd(_mm_set_sd(nand_), _mm_set_sd(VD(1.0))))));
    printf("maxsd 1,nan  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_max_sd(_mm_set_sd(VD(1.0)), _mm_set_sd(nand_)))));
    printf("minsd +0,-0  %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_min_sd(_mm_set_sd(VD(0.0)), _mm_set_sd(VD(-0.0))))));
    // packed: lanes exercise NaN / +-0 / ordered both directions at once
    __m128 a = loadps(nan, pz, two, three), b = loadps(VF(1.0f), nz, three, two);
    showps("minps       ", _mm_min_ps(a, b));
    showps("maxps       ", _mm_max_ps(a, b));
    __m128d ad = _mm_set_pd(VD(2.0), nand_), bd_ = _mm_set_pd(VD(3.0), VD(5.0));
    showpd("minpd       ", _mm_min_pd(ad, bd_));
    showpd("maxpd       ", _mm_max_pd(ad, bd_));
}

static void test_cmp(void) {
    float nan = mk_nanf();
    __m128 a = loadps(nan, VF(2.0f), VF(3.0f), VF(4.0f));
    __m128 b = loadps(VF(1.0f), VF(2.0f), VF(2.0f), VF(5.0f));
    // NLT = !(a<b) with NaN->true(all ones); NLE = !(a<=b) with NaN->true; also LT/LE/EQ/NEQ sanity.
    showps("cmpnltps    ", _mm_cmpnlt_ps(a, b));
    showps("cmpnleps    ", _mm_cmpnle_ps(a, b));
    showps("cmpltps     ", _mm_cmplt_ps(a, b));
    showps("cmpleps     ", _mm_cmple_ps(a, b));
    showps("cmpeqps     ", _mm_cmpeq_ps(a, b));
    showps("cmpneqps    ", _mm_cmpneq_ps(a, b));
    __m128d ad = _mm_set_pd(mk_nand(), VD(2.0)), bd_ = _mm_set_pd(VD(1.0), VD(2.0));
    showpd("cmpnltpd    ", _mm_cmpnlt_pd(ad, bd_));
    showpd("cmpnlepd    ", _mm_cmpnle_pd(ad, bd_));
    // scalar NLT/NLE with a NaN operand
    printf("cmpnltss nan %08x\n", bf(_mm_cvtss_f32(_mm_cmpnlt_ss(_mm_set_ss(mk_nanf()), _mm_set_ss(VF(1.0f))))));
    printf("cmpnless nan %08x\n", bf(_mm_cvtss_f32(_mm_cmpnle_ss(_mm_set_ss(mk_nanf()), _mm_set_ss(VF(1.0f))))));
}

static void test_cvt(void) {
    float nan = mk_nanf(); double nand_ = mk_nand();
    float pinf = infp_f(); double pinf_d = infp_d();
    // scalar float->int32 (cvtt = trunc, cvt = round). x86: out-of-range/NaN -> 0x80000000.
    printf("cvttss2si +ovf %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(VF(1e20f))));
    printf("cvttss2si -ovf %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(VF(-1e20f))));
    printf("cvttss2si nan  %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(nan)));
    printf("cvttss2si +inf %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(pinf)));
    printf("cvttss2si 2.9  %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(VF(2.9f))));
    printf("cvttss2si -2.9 %08x\n", (unsigned)_mm_cvtt_ss2si(_mm_set_ss(VF(-2.9f))));
    printf("cvtss2si  2.9  %08x\n", (unsigned)_mm_cvt_ss2si(_mm_set_ss(VF(2.9f))));      // round -> 3
    printf("cvtss2si  imax %08x\n", (unsigned)_mm_cvt_ss2si(_mm_set_ss(VF(2147483520.f))));
    printf("cvttsd2si +ovf %08x\n", (unsigned)_mm_cvttsd_si32(_mm_set_sd(VD(1e20))));
    printf("cvttsd2si nan  %08x\n", (unsigned)_mm_cvttsd_si32(_mm_set_sd(nand_)));
    printf("cvttsd2si -ovf %08x\n", (unsigned)_mm_cvttsd_si32(_mm_set_sd(VD(-1e20))));
    // scalar float->int64
    printf("cvttss2si64 ov %016llx\n", (unsigned long long)_mm_cvttss_si64(_mm_set_ss(VF(1e30f))));
    printf("cvttss2si64 nan%016llx\n", (unsigned long long)_mm_cvttss_si64(_mm_set_ss(nan)));
    printf("cvttsd2si64 ov %016llx\n", (unsigned long long)_mm_cvttsd_si64(_mm_set_sd(VD(1e30))));
    printf("cvttsd2si64 in %016llx\n", (unsigned long long)_mm_cvttsd_si64(_mm_set_sd(pinf_d)));
    // packed float->int32
    __m128 v = loadps(VF(1e20f), nan, VF(-1e20f), VF(2.9f));
    showdq("cvttps2dq   ", _mm_cvttps_epi32(v));
    showdq("cvtps2dq    ", _mm_cvtps_epi32(v));
}

__attribute__((target("sse4.1"))) static void test_round(void) {
    // MXCSR.RC = round-down, then ROUND with CUR_DIRECTION (uses MXCSR) vs explicit NEAREST (ignores it).
    _MM_SET_ROUNDING_MODE(_MM_ROUND_DOWN);
    __m128 v = loadps(VF(2.7f), VF(-2.7f), VF(2.5f), VF(3.5f));
    showps("round cur   ", _mm_round_ps(v, _MM_FROUND_CUR_DIRECTION));       // MXCSR=down
    showps("round nrst  ", _mm_round_ps(v, _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC));
    showps("round down  ", _mm_round_ps(v, _MM_FROUND_TO_NEG_INF | _MM_FROUND_NO_EXC));
    showps("round up    ", _mm_round_ps(v, _MM_FROUND_TO_POS_INF | _MM_FROUND_NO_EXC));
    showps("round trunc ", _mm_round_ps(v, _MM_FROUND_TO_ZERO | _MM_FROUND_NO_EXC));
    __m128d dv = _mm_set_pd(VD(-2.7), VD(2.7));
    showpd("roundpd cur ", _mm_round_pd(dv, _MM_FROUND_CUR_DIRECTION));
    showpd("roundpd nrst", _mm_round_pd(dv, _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC));
    printf("roundss cur  %08x\n", bf(_mm_cvtss_f32(_mm_round_ss(_mm_setzero_ps(), _mm_set_ss(VF(2.7f)), _MM_FROUND_CUR_DIRECTION))));
    printf("roundsd nrst %016llx\n", (unsigned long long)bd(_mm_cvtsd_f64(_mm_round_sd(_mm_setzero_pd(), _mm_set_sd(VD(2.7)), _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC))));
    _MM_SET_ROUNDING_MODE(_MM_ROUND_NEAREST); // restore
}

int main(void) {
    (void)g_i;
    test_minmax();
    test_cmp();
    test_cvt();
    test_round();
    return 0;
}
