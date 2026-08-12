/*
 * roundeven(3) and roundevenf(3) for a runtime that does not ship them.
 *
 * These are not an engine abstraction and there is no seam here -- they are two
 * C23 <math.h> functions that mingw-w64's libm does not export. Clang lowers
 * __builtin_roundeven to a call when it cannot select a single instruction, and
 * the x86-64 guest's SSE4.1 ROUNDPD/ROUNDPS emulation uses that builtin
 * deliberately: those instructions round half-to-even INDEPENDENTLY of the
 * current rounding mode, which is exactly what roundeven means and exactly what
 * rint/nearbyint do not do (they follow the mode).
 *
 * So the semantics matter and cannot be approximated by the nearest available
 * libm call:
 *
 *   - round(3) breaks ties AWAY FROM ZERO. 0.5 -> 1, 2.5 -> 3. Wrong.
 *   - rint(3)/nearbyint(3) break ties to even ONLY while the mode is
 *     FE_TONEAREST, and this code runs with the mode set from the guest's MXCSR.
 *     Right answer, wrong reason, and wrong whenever the guest has selected a
 *     directed mode -- which is the case the emulation exists to serve.
 *
 * The implementation below is exact for every finite input and is mode
 * independent, because it only uses floor (which is mode independent) and exact
 * comparisons. NaN, the infinities and any value already integral fall out of
 * the first branch unchanged, including the sign of a negative zero.
 */

#include <math.h>

double roundeven(double x) {
    double truncated;
    double fraction;
    /* Beyond 2^52 every double is an integer, and floor/fmod on such values are
     * identities -- returning early also preserves NaN payloads and the sign of
     * -0.0, which the arithmetic below would not. */
    if (!(fabs(x) < 4503599627370496.0)) return x;
    truncated = floor(x);
    fraction = x - truncated;
    if (fraction > 0.5) return truncated + 1.0;
    if (fraction < 0.5) return truncated;
    /* Exactly halfway: choose the even neighbour. floor() is below x, so the
     * candidates are truncated and truncated+1. */
    return fmod(truncated, 2.0) == 0.0 ? truncated : truncated + 1.0;
}

float roundevenf(float x) {
    float truncated;
    float fraction;
    if (!(fabsf(x) < 8388608.0f)) return x; /* 2^23 */
    truncated = floorf(x);
    fraction = x - truncated;
    if (fraction > 0.5f) return truncated + 1.0f;
    if (fraction < 0.5f) return truncated;
    return fmodf(truncated, 2.0f) == 0.0f ? truncated : truncated + 1.0f;
}
