#ifndef HL_LINUX_ABI_IMAGE_H
#define HL_LINUX_ABI_IMAGE_H

#include "hl/host_services.h"

#include <stddef.h>
#include <stdint.h>

/* Host-neutral, owned input image used by the Linux ELF loaders. */
typedef struct hl_linux_image {
    uint8_t *bytes;
    size_t size;
} hl_linux_image;

void hl_linux_image_release(hl_linux_image *image);
int hl_linux_image_read(const hl_host_services *host, const char *path, hl_linux_image *image);
int hl_linux_image_read_handle(const hl_host_services *host, hl_host_handle handle, hl_linux_image *image);
int hl_linux_image_read_bytes(const void *bytes, size_t size, hl_linux_image *image);
int hl_linux_image_read_fd(int descriptor, hl_linux_image *image);

#endif
