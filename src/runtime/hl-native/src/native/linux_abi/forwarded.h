#ifndef HL_LINUX_ABI_FORWARDED_H
#define HL_LINUX_ABI_FORWARDED_H

/* Canonical syscalls serviced by the sentry authority boundary. */
#define HL_LINUX_SENTRY_FORWARDED(X)                                                                                   \
    X(19)                                                                                                              \
    X(20)                                                                                                              \
    X(21)                                                                                                              \
    X(22)                                                                                                              \
    X(23)                                                                                                              \
    X(24)                                                                                                              \
    X(25)                                                                                                              \
    X(29)                                                                                                              \
    X(32)                                                                                                              \
    X(46)                                                                                                              \
    X(47)                                                                                                              \
    X(48)                                                                                                              \
    X(50)                                                                                                              \
    X(52)                                                                                                              \
    X(55)                                                                                                              \
    X(56)                                                                                                              \
    X(57)                                                                                                              \
    X(59)                                                                                                              \
    X(61)                                                                                                              \
    X(62)                                                                                                              \
    X(63)                                                                                                              \
    X(64)                                                                                                              \
    X(65)                                                                                                              \
    X(66)                                                                                                              \
    X(67)                                                                                                              \
    X(68)                                                                                                              \
    X(71)                                                                                                              \
    X(72)                                                                                                              \
    X(73)                                                                                                              \
    X(76)                                                                                                              \
    X(78)                                                                                                              \
    X(79)                                                                                                              \
    X(80)                                                                                                              \
    X(82)                                                                                                              \
    X(83)                                                                                                              \
    X(84)                                                                                                              \
    X(198)                                                                                                             \
    X(199)                                                                                                             \
    X(200)                                                                                                             \
    X(201)                                                                                                             \
    X(202)                                                                                                             \
    X(203)                                                                                                             \
    X(204)                                                                                                             \
    X(205)                                                                                                             \
    X(206)                                                                                                             \
    X(207)                                                                                                             \
    X(208)                                                                                                             \
    X(209)                                                                                                             \
    X(210)                                                                                                             \
    X(211)                                                                                                             \
    X(212)                                                                                                             \
    X(242)                                                                                                             \
    X(267)                                                                                                             \
    X(279)                                                                                                             \
    X(285)                                                                                                             \
    X(291)                                                                                                             \
    X(436)                                                                                                             \
    X(439)

/* Forwarded calls whose complete request is the six scalar argument words.  These still need an
 * explicit transport shape: an importer returning zero means "not admitted", not "no bytes to copy".
 * Keep this list separate from HL_LINUX_SENTRY_FORWARDED so adding a pointer-bearing syscall cannot
 * accidentally cross the authority boundary without a marshal implementation. */
#define HL_LINUX_SENTRY_SCALAR(X)                                                                                      \
    X(19)                                                                                                              \
    X(20)                                                                                                              \
    X(23)                                                                                                              \
    X(24)                                                                                                              \
    X(32)                                                                                                              \
    X(46)                                                                                                              \
    X(47)                                                                                                              \
    X(50)                                                                                                              \
    X(52)                                                                                                              \
    X(55)                                                                                                              \
    X(57)                                                                                                              \
    X(62)                                                                                                              \
    X(82)                                                                                                              \
    X(83)                                                                                                              \
    X(84)                                                                                                              \
    X(198)                                                                                                             \
    X(201)                                                                                                             \
    X(210)                                                                                                             \
    X(267)                                                                                                             \
    X(436)

#endif
