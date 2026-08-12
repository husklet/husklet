#ifndef HL_CORE_PROVIDER_TREE_FILES_H
#define HL_CORE_PROVIDER_TREE_FILES_H

#include "client.h"
#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>

enum { HL_PROVIDER_TREE_FILE = 0, HL_PROVIDER_TREE_DIRECTORY = 1, HL_PROVIDER_TREE_LINK = 2 };

int hl_provider_tree_files_install(hl_host_services *services, hl_provider_client *client);
void hl_provider_tree_files_revoke(void);
int hl_provider_tree_files_active(void);
hl_host_result hl_provider_tree_open_root(const char *path, size_t path_size, uint32_t access, uint32_t creation,
                                          uint32_t permissions, uint32_t kind);

#endif
