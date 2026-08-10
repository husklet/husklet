#ifndef HL_LINUX_FDCACHE_H
#define HL_LINUX_FDCACHE_H

void hl_fdcache_runtime_init(void);

#include "container/namespace.h"
#include "fdhandle.h" /* the other descriptor-keyed side table this TU owns */
#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/stat.h>
#include <sys/types.h>

#define HL_FDCACHE_PATH_CAPACITY 192u

typedef struct hl_fdcache_binding {
    const hl_host_services *host;
    const struct hl_linux_vfs_namespace *vfs;
    const int *volume_count;
    const char *root_canonical;
    const size_t *root_canonical_length;
    char (*fd_paths)[HL_FDCACHE_PATH_CAPACITY];
    size_t fd_capacity;
    const int *threaded;
    const char *generation_file;
} hl_fdcache_binding;

int hl_fdcache_bind(const hl_fdcache_binding *binding);

int hl_fdcache_metadata_lookup(const char *path, int *result, struct stat *metadata);
void hl_fdcache_metadata_store(const char *path, int result, const struct stat *metadata);
void hl_fdcache_metadata_evict(const char *path);
void hl_fdcache_metadata_evict_inode(dev_t device, ino_t inode);
int hl_fdcache_readlink_lookup(const char *path, int *result, char *output, int capacity, int *length);
void hl_fdcache_readlink_store(const char *path, int result, const char *link, int length);
void hl_fdcache_readlink_evict(const char *path);
int hl_fdcache_access_lookup(const char *path, int *result);
void hl_fdcache_access_store(const char *path, int result);
void hl_fdcache_access_evict(const char *path);
void hl_fdcache_resolution_bump(void);
void hl_fdcache_reset(void);
int hl_fdcache_resolution_lookup(const char *guest, char *host, size_t capacity);
void hl_fdcache_resolution_store(const char *guest, const char *host);
int hl_fdcache_upper_negative_lookup(const char *directory);
void hl_fdcache_upper_negative_store(const char *directory);
int hl_fdcache_upper_verdict_lookup(const char *directory, int *verdict);
void hl_fdcache_upper_verdict_store(const char *directory, int verdict);
int hl_fdcache_dentry_cacheable(const char *jail_canonical);
int hl_fdcache_dentry_lookup(const char *key, char *canonical, size_t capacity, int *missing);
void hl_fdcache_dentry_store(const char *key, const char *canonical, int missing);
int hl_fdcache_open_lookup(const char *guest, char *host, size_t capacity);
void hl_fdcache_open_store(const char *guest, const char *host);
void hl_fdcache_generation_poll(void);
void hl_fdcache_fd_setpath(int fd, const char *path);
void hl_fdcache_fd_evict(int fd);
void hl_fdcache_fd_clear(int fd);
void hl_fdcache_evict_path(const char *host_path);

#endif
