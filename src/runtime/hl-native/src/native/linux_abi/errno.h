#ifndef HL_LINUX_ERRNO_H
#define HL_LINUX_ERRNO_H

/* Convert a positive errno from the current host C runtime into Linux's
 * guest-visible errno namespace. This is intentionally not valid for values
 * that already came from an hl_linux_* API. */
int hl_linux_errno_from_host(int host_errno);
int hl_linux_errno_from_darwin(int host_errno);
int hl_linux_errno_from_ucrt(int host_errno);

#endif
