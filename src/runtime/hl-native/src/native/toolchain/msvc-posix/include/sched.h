/*
 * <sched.h> for the x86_64-pc-windows-msvc target.
 *
 * One function is reached from this tree: sched_yield(), used by the spin
 * paths in the dispatch and provider layers. Win32's SwitchToThread() is the
 * exact counterpart -- it yields the remainder of the time slice to another
 * ready thread on the same processor -- so posix.c implements it over that
 * rather than over Sleep(0), which differs in that it will not yield to a
 * lower-priority thread.
 *
 * No CPU-affinity surface is declared. sched_getaffinity and cpu_set_t exist
 * in this tree only behind __linux__.
 */

#ifndef HL_MSVC_POSIX_SCHED_H
#define HL_MSVC_POSIX_SCHED_H

#ifdef __cplusplus
extern "C" {
#endif

int sched_yield(void);

#ifdef __cplusplus
}
#endif

#endif /* HL_MSVC_POSIX_SCHED_H */
