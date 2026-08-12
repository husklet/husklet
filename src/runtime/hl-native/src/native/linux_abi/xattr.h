#ifndef HL_LINUX_XATTR_H
#define HL_LINUX_XATTR_H

#include <stdint.h>

int hl_xattr_cache_is_negative(uint64_t device, uint64_t inode);
void hl_xattr_cache_record_negative(uint64_t device, uint64_t inode);
void hl_xattr_cache_invalidate(void);

#endif
