#ifndef HL_HOST_WINDOWS_WIN32_H
#define HL_HOST_WINDOWS_WIN32_H

/*
 * The ONE place <windows.h> is allowed to enter the guest-target unity
 * translation unit.
 *
 * The rest of this tree goes to some length to avoid that include: native
 * compatibility headers declare the handful of Win32 entry points they call by
 * hand instead, precisely so the Win32 preprocessor vocabulary does not land in
 * the same TU that defines the guest ABI. That remains the right default and
 * this header does not repeal it.
 *
 * What forces the exception is the fault context. A vectored exception handler
 * receives a CONTEXT record, and the engine's signal-context accessors have to
 * read and WRITE its fields (Rip, Rsp, the GPR block). A field access needs the
 * complete type, so it cannot be hand-declared the way a function can. Once
 * CONTEXT is in the TU, <windows.h> is in the TU.
 *
 * So the include is made once, here, with the damage bounded on both sides:
 *
 *   - the feature macros below suppress the largest offenders BEFORE the
 *     include. NOMINMAX in particular is not optional: without it windows.h
 *     defines min and max as function-like macros, and this tree has many
 *     identifiers spelled that way.
 *   - the #undef block after the include removes the object-like macros that
 *     have no business existing in a C translation unit at all. IN, OUT and
 *     OPTIONAL are defined as EMPTY -- they are SAL-era annotations -- so a
 *     variable or field named `in` is safe but one named `IN` silently
 *     vanishes. DELETE, ERROR, ABSOLUTE and RELATIVE are enum-shaped names this
 *     tree may want for its own enumerators. `interface`, `near`, `far` and
 *     `small` are 16-bit-era keywords emulated as macros.
 *
 * Removing a macro cannot break Win32 code that has already been preprocessed,
 * and no Win32 header is included after this point, so the #undefs are safe
 * rather than merely convenient. If a later Win32 header genuinely needs one of
 * these, include it BEFORE this file rather than shortening this list.
 *
 * Non-Windows hosts get an empty header, so an unconditional include at the top
 * of a shared target root costs them nothing.
 */

#if defined(_WIN32)

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN 1
#endif
#ifndef NOMINMAX
#define NOMINMAX 1
#endif
#ifndef NOGDI
#define NOGDI 1
#endif
#ifndef NOUSER
/* No USER32 surface reaches this TU, and the import gate forbids one. Excluding
 * the declarations here means a call that would have imported USER32 fails at
 * COMPILE time, in this TU, instead of at link time in an archive consumer. */
#define NOUSER 1
#endif
#ifndef NOSERVICE
#define NOSERVICE 1
#endif
#ifndef NOMCX
#define NOMCX 1
#endif
#ifndef NOIME
#define NOIME 1
#endif

#include <windows.h>

/* SAL-era annotation macros expanding to nothing -- the dangerous class. */
#undef IN
#undef OUT
#undef OPTIONAL
/* Enum-shaped names windows.h claims for itself. */
#undef DELETE
#undef ERROR
#undef ABSOLUTE
#undef RELATIVE
/* 16-bit-era keyword emulation. */
#undef interface
#undef near
#undef far
#undef small
#undef pascal

#endif /* _WIN32 */

#endif
