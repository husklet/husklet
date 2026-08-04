#ifndef HL_NATIVE_TOOLCHAIN_H
#define HL_NATIVE_TOOLCHAIN_H

#if defined(_FORTIFY_SOURCE) && !defined(__OPTIMIZE__)
#undef _FORTIFY_SOURCE
#endif

#endif
