#ifndef HL_LINUX_VFS_ROUTE_H
#define HL_LINUX_VFS_ROUTE_H

#include <stddef.h>

static const char *atpath(int directory_fd, const char *raw, char *buffer, size_t size, int nofollow);

#endif
