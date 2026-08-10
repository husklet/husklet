#ifndef HL_HOST_FILE_H
#define HL_HOST_FILE_H

#include "hl/host_services.h"

int hl_host_file_create(const hl_host_services *services, const char *path, uint32_t permissions);
int hl_host_file_exclusive(const hl_host_services *services, const char *path, uint32_t permissions);
int hl_host_file_reset(const hl_host_services *services, const char *path, uint32_t permissions);
int hl_host_file_store(const hl_host_services *services, const char *path, uint32_t permissions, const void *data,
                       size_t size);
int hl_host_file_load(const hl_host_services *services, const char *path, void *data, size_t size);
int hl_host_file_rename(const hl_host_services *services, const char *old_path, const char *new_path);
int hl_host_file_unlink(const hl_host_services *services, const char *path);

#endif
