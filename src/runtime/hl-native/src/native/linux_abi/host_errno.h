#ifndef HL_LINUX_ABI_HOST_ERRNO_H
#define HL_LINUX_ABI_HOST_ERRNO_H

#include <errno.h>

#if EAGAIN == EWOULDBLOCK
#define HL_HOST_ERRNO_WOULD_BLOCK(error) ((error) == EAGAIN)
#else
#define HL_HOST_ERRNO_WOULD_BLOCK(error) ((error) == EAGAIN || (error) == EWOULDBLOCK)
#endif

#endif
