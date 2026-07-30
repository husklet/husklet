#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#include "hl_surface_buffer.h"

extern int hl_shim_external_buffers_enabled(void);

struct gbm_device {
    int fd;
};

union gbm_bo_handle {
    void *ptr;
    int32_t s32;
    uint32_t u32;
    int64_t s64;
    uint64_t u64;
};

struct gbm_bo {
    struct gbm_device *device;
    int fd;
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t stride;
    uint64_t modifier;
    uint64_t offset;
    uint32_t usage;
    size_t size;
};

struct gbm_import_fd_data {
    int fd;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t format;
};

struct gbm_import_fd_modifier_data {
    uint32_t width;
    uint32_t height;
    uint32_t format;
    uint32_t num_fds;
    int fds[4];
    int strides[4];
    int offsets[4];
    uint64_t modifier;
};

enum {
    GBM_BO_IMPORT_FD = 0x5503,
    GBM_BO_IMPORT_FD_MODIFIER = 0x5504,
    GBM_BO_USE_SCANOUT = 1 << 0,
    GBM_BO_USE_CURSOR = 1 << 1,
    GBM_BO_USE_RENDERING = 1 << 2,
    GBM_BO_USE_WRITE = 1 << 3,
    GBM_BO_USE_LINEAR = 1 << 4,
    GBM_BO_USE_PROTECTED = 1 << 5,
    GBM_BO_USE_FRONT_RENDERING = 1 << 6,
    GBM_BO_TRANSFER_READ = 1 << 0,
    GBM_BO_TRANSFER_WRITE = 1 << 1,
};

enum {
    GBM_FORMAT_XRGB8888 = 0x34325258,
    GBM_FORMAT_ARGB8888 = 0x34325241,
};

static const uint32_t supported_usage =
    GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING | GBM_BO_USE_WRITE |
    GBM_BO_USE_LINEAR | GBM_BO_USE_FRONT_RENDERING;

static int external_enabled(void) {
    return hl_shim_external_buffers_enabled();
}

static void put_le16(unsigned char *out, uint16_t value) {
    out[0] = (unsigned char)value;
    out[1] = (unsigned char)(value >> 8);
}

static void put_le32(unsigned char *out, uint32_t value) {
    for (unsigned index = 0; index < 4; ++index)
        out[index] = (unsigned char)(value >> (index * 8));
}

static void put_le64(unsigned char *out, uint64_t value) {
    for (unsigned index = 0; index < 8; ++index)
        out[index] = (unsigned char)(value >> (index * 8));
}

static uint16_t get_le16(const unsigned char *in) {
    return (uint16_t)in[0] | (uint16_t)in[1] << 8;
}

static uint32_t get_le32(const unsigned char *in) {
    uint32_t value = 0;
    for (unsigned index = 0; index < 4; ++index)
        value |= (uint32_t)in[index] << (index * 8);
    return value;
}

static uint64_t get_le64(const unsigned char *in) {
    uint64_t value = 0;
    for (unsigned index = 0; index < 8; ++index)
        value |= (uint64_t)in[index] << (index * 8);
    return value;
}

static int header_write(int fd, uint64_t token, uint32_t width,
                        uint32_t height, uint32_t format, uint32_t stride,
                        uint64_t allocation_size) {
    const unsigned char magic[8] = HL_SURFACE_MAGIC;
    unsigned char header[HL_SURFACE_HEADER_LEN] = {0};
    memcpy(header, magic, sizeof(magic));
    put_le16(header + 8, HL_SURFACE_VERSION);
    put_le16(header + 10, HL_SURFACE_HEADER_LEN);
    put_le64(header + 16, token);
    put_le64(header + 24, 0);
    put_le32(header + 32, width);
    put_le32(header + 36, height);
    put_le32(header + 40, format);
    put_le32(header + 44, stride);
    put_le64(header + 48, HL_SURFACE_PLANE_OFFSET);
    put_le64(header + 56, allocation_size);
    uint64_t header_offset = (uint64_t)stride * height;
    return header_offset <= INT64_MAX &&
           pwrite(fd, header, sizeof(header), (off_t)header_offset) ==
               (ssize_t)sizeof(header);
}

static int header_matches(int fd, uint32_t width, uint32_t height,
                          uint32_t format, uint32_t stride, uint64_t offset,
                          uint64_t allocation_size) {
    const unsigned char magic[8] = HL_SURFACE_MAGIC;
    unsigned char header[HL_SURFACE_HEADER_LEN];
    uint64_t plane_size = (uint64_t)stride * height;
    uint64_t header_offset = offset + plane_size;
    struct stat stat;
    if (header_offset > INT64_MAX ||
        fstat(fd, &stat) != 0 || stat.st_size < 0 ||
        (uint64_t)stat.st_size != allocation_size ||
        pread(fd, header, sizeof(header), (off_t)header_offset) !=
            (ssize_t)sizeof(header) ||
        memcmp(header, magic, sizeof(magic)) != 0 ||
        get_le16(header + 8) != HL_SURFACE_VERSION ||
        get_le16(header + 10) != HL_SURFACE_HEADER_LEN ||
        get_le32(header + 12) != 0 || get_le64(header + 16) == 0 ||
        get_le32(header + 32) != width || get_le32(header + 36) != height ||
        get_le32(header + 40) != format || get_le32(header + 44) != stride ||
        get_le64(header + 48) != offset ||
        get_le64(header + 56) != allocation_size) {
        errno = EINVAL;
        return 0;
    }
    return 1;
}

static uint64_t random_token(void) {
    uint64_t token = 0;
    while (!token) {
        ssize_t read =
            syscall(SYS_getrandom, &token, sizeof(token), 0);
        if (read != (ssize_t)sizeof(token)) return 0;
    }
    return token;
}

static void debug(const char *message, size_t length) {
    if (getenv("HL_SHIM_DEBUG")) {
        ssize_t ignored = write(STDERR_FILENO, message, length);
        (void)ignored;
    }
}

static void debugf(const char *format, ...) {
    if (!getenv("HL_SHIM_DEBUG")) return;
    char message[384];
    va_list arguments;
    va_start(arguments, format);
    int length = vsnprintf(message, sizeof(message), format, arguments);
    va_end(arguments);
    if (length <= 0) return;
    size_t bytes = (size_t)length;
    if (bytes >= sizeof(message)) bytes = sizeof(message) - 1;
    debug(message, bytes);
}

static uint32_t aligned_stride(uint32_t width) {
    uint64_t bytes = (uint64_t)width * 4;
    return bytes > UINT32_MAX ? 0 : (uint32_t)((bytes + 63) & ~UINT64_C(63));
}

static int duplicate_fd(int fd) {
    if (fd < 0) {
        errno = EBADF;
        return -1;
    }
    return fcntl(fd, F_DUPFD_CLOEXEC, 0);
}

static int format_supported(uint32_t format) {
    return format == GBM_FORMAT_ARGB8888 || format == GBM_FORMAT_XRGB8888;
}

static int usage_supported(uint32_t usage) {
    return (usage & ~supported_usage) == 0 &&
           !(usage & (GBM_BO_USE_CURSOR | GBM_BO_USE_PROTECTED));
}

static int backing_supported(int fd, uint32_t width, uint32_t height,
                             uint32_t stride, uint32_t format,
                             uint64_t offset) {
    if (fd < 0 || !width || !height || !stride ||
        !format_supported(format) || stride < (uint64_t)width * 4) {
        errno = EINVAL;
        return 0;
    }

    uint64_t bytes = (uint64_t)stride * height;
    if (bytes > SIZE_MAX || offset > UINT64_MAX - bytes ||
        offset + bytes > INT64_MAX) {
        errno = EOVERFLOW;
        return 0;
    }

    struct stat stat;
    if (fstat(fd, &stat) != 0) return 0;
    if (stat.st_size < 0 || (uint64_t)stat.st_size < offset + bytes) {
        errno = EINVAL;
        return 0;
    }
    return 1;
}

static struct gbm_bo *bo_new(struct gbm_device *device, int fd, uint32_t width,
                             uint32_t height, uint32_t stride, uint32_t format,
                             uint64_t modifier, uint64_t offset,
                             uint32_t usage) {
    if (!device || fd < 0 || !usage_supported(usage)) {
        if (fd >= 0) close(fd);
        errno = EINVAL;
        return NULL;
    }
    if (!backing_supported(fd, width, height, stride, format, offset)) {
        close(fd);
        return NULL;
    }
    uint64_t size = (uint64_t)stride * height;
    if (size > SIZE_MAX) {
        close(fd);
        errno = EOVERFLOW;
        return NULL;
    }
    struct gbm_bo *bo = calloc(1, sizeof(*bo));
    if (!bo) {
        close(fd);
        return NULL;
    }
    bo->device = device;
    bo->fd = fd;
    bo->width = width;
    bo->height = height;
    bo->stride = stride;
    bo->format = format;
    bo->modifier = modifier;
    bo->offset = offset;
    bo->usage = usage;
    bo->size = (size_t)size;
    return bo;
}

static int external_usage(uint32_t usage) {
    return external_enabled() &&
           (usage & (GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING)) != 0 &&
           (usage & (GBM_BO_USE_LINEAR | GBM_BO_USE_WRITE)) == 0;
}

static struct gbm_bo *bo_create_external(
    struct gbm_device *device, uint32_t width, uint32_t height,
    uint32_t format, uint32_t usage) {
    uint32_t stride = aligned_stride(width);
    uint64_t plane_size = (uint64_t)stride * height;
    uint64_t allocation_size = plane_size + HL_SURFACE_HEADER_LEN;
    if (!device || !stride || !height || !format_supported(format) ||
        !usage_supported(usage) || !external_usage(usage) ||
        allocation_size > INT64_MAX) {
        errno = EINVAL;
        return NULL;
    }
    int fd = (int)syscall(SYS_memfd_create, "hl-gbm-external", 1);
    uint64_t token = random_token();
    if (fd < 0 || token == 0 ||
        ftruncate(fd, (off_t)allocation_size) != 0 ||
        !header_write(fd, token, width, height, format, stride,
                      allocation_size)) {
        if (fd >= 0) close(fd);
        return NULL;
    }
    return bo_new(device, fd, width, height, stride, format,
                  HL_SURFACE_MODIFIER, HL_SURFACE_PLANE_OFFSET, usage);
}

struct gbm_device *gbm_create_device(int fd) {
    static const char entered[] = "[hl-gbm-shim] gbm_create_device entered\n";
    static const char succeeded[] = "[hl-gbm-shim] gbm_create_device succeeded\n";
    static const char failed[] = "[hl-gbm-shim] gbm_create_device failed\n";
    debug(entered, sizeof(entered) - 1);
    if (fd < 0) {
        errno = EBADF;
        debug(failed, sizeof(failed) - 1);
        return NULL;
    }
    struct gbm_device *device = calloc(1, sizeof(*device));
    if (!device) {
        debug(failed, sizeof(failed) - 1);
        return NULL;
    }
    device->fd = duplicate_fd(fd);
    if (device->fd < 0) {
        free(device);
        debug(failed, sizeof(failed) - 1);
        return NULL;
    }
    debug(succeeded, sizeof(succeeded) - 1);
    return device;
}

void gbm_device_destroy(struct gbm_device *device) {
    debugf("[hl-gbm-shim] gbm_device_destroy device=%p\n", (void *)device);
    if (!device) return;
    close(device->fd);
    free(device);
}

int gbm_device_get_fd(struct gbm_device *device) {
    int fd = device ? device->fd : -1;
    debugf("[hl-gbm-shim] gbm_device_get_fd device=%p fd=%d\n",
           (void *)device, fd);
    return fd;
}

const char *gbm_device_get_backend_name(struct gbm_device *device) {
    const char *name = device ? "husklet" : NULL;
    debugf("[hl-gbm-shim] gbm_device_get_backend_name device=%p name=%s\n",
           (void *)device, name ? name : "(null)");
    return name;
}

int gbm_device_is_format_supported(struct gbm_device *device, uint32_t format,
                                   uint32_t usage) {
    int supported =
        device && format_supported(format) && usage_supported(usage);
    debugf("[hl-gbm-shim] gbm_device_is_format_supported format=0x%08x "
           "usage=0x%08x supported=%d\n",
           format, usage, supported);
    return supported;
}

int gbm_device_get_format_modifier_plane_count(struct gbm_device *device,
                                               uint32_t format,
                                               uint64_t modifier) {
    int planes = device && format_supported(format) &&
                         (modifier == 0 ||
                          (modifier == HL_SURFACE_MODIFIER &&
                           external_enabled()))
                     ? 1
                     : 0;
    debugf("[hl-gbm-shim] gbm_device_get_format_modifier_plane_count "
           "format=0x%08x modifier=0x%016llx planes=%d\n",
           format, (unsigned long long)modifier, planes);
    return planes;
}

struct gbm_bo *gbm_bo_create(struct gbm_device *device, uint32_t width,
                             uint32_t height, uint32_t format, uint32_t usage) {
    debugf("[hl-gbm-shim] gbm_bo_create width=%u height=%u format=0x%08x "
           "usage=0x%08x\n",
           width, height, format, usage);
    uint32_t stride = aligned_stride(width);
    if (!device || !stride || !height || !format_supported(format) ||
        !usage_supported(usage)) {
        errno = EINVAL;
        return NULL;
    }
    int fd = (int)syscall(SYS_memfd_create, "hl-gbm", 1);
    uint64_t size = (uint64_t)stride * height;
    if (fd < 0 || size > INT64_MAX || ftruncate(fd, (off_t)size) != 0) {
        if (fd >= 0) close(fd);
        return NULL;
    }
    struct gbm_bo *bo =
        bo_new(device, fd, width, height, stride, format, 0, 0, usage);
    debugf("[hl-gbm-shim] gbm_bo_create result=%p errno=%d stride=%u\n",
           (void *)bo, bo ? 0 : errno, stride);
    return bo;
}

struct gbm_bo *gbm_bo_create_with_modifiers(struct gbm_device *device,
                                            uint32_t width, uint32_t height,
                                            uint32_t format,
                                            const uint64_t *modifiers,
                                            unsigned count) {
    debugf("[hl-gbm-shim] gbm_bo_create_with_modifiers width=%u height=%u "
           "format=0x%08x count=%u\n",
           width, height, format, count);
    if (!modifiers || !count) {
        errno = EINVAL;
        return NULL;
    }
    for (unsigned index = 0; index < count; ++index)
        if (modifiers[index] == HL_SURFACE_MODIFIER && external_enabled())
            return bo_create_external(device, width, height, format,
                                      GBM_BO_USE_RENDERING);
    for (unsigned index = 0; index < count; ++index)
        if (modifiers[index] == 0)
            return gbm_bo_create(device, width, height, format, 0);
    errno = ENOTSUP;
    return NULL;
}

struct gbm_bo *gbm_bo_create_with_modifiers2(
    struct gbm_device *device, uint32_t width, uint32_t height, uint32_t format,
    const uint64_t *modifiers, unsigned count, uint32_t flags) {
    debugf("[hl-gbm-shim] gbm_bo_create_with_modifiers2 flags=0x%08x\n",
           flags);
    if (!modifiers || !count) {
        errno = EINVAL;
        return NULL;
    }
    for (unsigned index = 0; index < count; ++index)
        if (modifiers[index] == HL_SURFACE_MODIFIER &&
            external_usage(flags))
            return bo_create_external(device, width, height, format, flags);
    for (unsigned index = 0; index < count; ++index)
        if (modifiers[index] == 0)
            return gbm_bo_create(device, width, height, format, flags);
    errno = ENOTSUP;
    return NULL;
}

struct gbm_bo *gbm_bo_import(struct gbm_device *device, uint32_t type,
                             void *buffer, uint32_t usage) {
    debugf("[hl-gbm-shim] gbm_bo_import type=0x%08x usage=0x%08x "
           "buffer=%p\n",
           type, usage, buffer);
    if (type == GBM_BO_IMPORT_FD && buffer) {
        struct gbm_import_fd_data *data = buffer;
        if (!usage_supported(usage) ||
            !backing_supported(data->fd, data->width, data->height,
                               data->stride, data->format, 0))
            return NULL;
        int fd = duplicate_fd(data->fd);
        if (fd < 0) return NULL;
        return bo_new(device, fd, data->width, data->height, data->stride,
                      data->format, 0, 0, usage);
    }
    if (type == GBM_BO_IMPORT_FD_MODIFIER && buffer) {
        struct gbm_import_fd_modifier_data *data = buffer;
        if (!usage_supported(usage) || data->num_fds != 1 ||
            data->strides[0] <= 0 ||
            data->offsets[0] < 0) {
            errno = ENOTSUP;
            return NULL;
        }
        if (!backing_supported(data->fds[0], data->width, data->height,
                               (uint32_t)data->strides[0], data->format,
                               (uint32_t)data->offsets[0]))
            return NULL;
        uint64_t offset = (uint32_t)data->offsets[0];
        uint64_t plane_size =
            (uint64_t)(uint32_t)data->strides[0] * data->height;
        uint64_t allocation_size = offset + plane_size;
        if (data->modifier == HL_SURFACE_MODIFIER) {
            if (allocation_size > UINT64_MAX - HL_SURFACE_HEADER_LEN) {
                errno = EOVERFLOW;
                return NULL;
            }
            allocation_size += HL_SURFACE_HEADER_LEN;
            if (!external_usage(usage) ||
                offset != HL_SURFACE_PLANE_OFFSET ||
                !header_matches(data->fds[0], data->width, data->height,
                                data->format, (uint32_t)data->strides[0],
                                offset, allocation_size))
                return NULL;
        } else if (data->modifier != 0) {
            errno = ENOTSUP;
            return NULL;
        }
        int fd = duplicate_fd(data->fds[0]);
        if (fd < 0) return NULL;
        return bo_new(device, fd, data->width, data->height,
                      (uint32_t)data->strides[0], data->format, data->modifier,
                      offset, usage);
    }
    errno = ENOTSUP;
    return NULL;
}

void gbm_bo_destroy(struct gbm_bo *bo) {
    debugf("[hl-gbm-shim] gbm_bo_destroy bo=%p\n", (void *)bo);
    if (!bo) return;
    close(bo->fd);
    free(bo);
}

struct gbm_device *gbm_bo_get_device(struct gbm_bo *bo) {
    struct gbm_device *device = bo ? bo->device : NULL;
    debugf("[hl-gbm-shim] gbm_bo_get_device bo=%p device=%p\n",
           (void *)bo, (void *)device);
    return device;
}

uint32_t gbm_bo_get_width(struct gbm_bo *bo) {
    uint32_t width = bo ? bo->width : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_width bo=%p width=%u\n",
           (void *)bo, width);
    return width;
}
uint32_t gbm_bo_get_height(struct gbm_bo *bo) {
    uint32_t height = bo ? bo->height : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_height bo=%p height=%u\n",
           (void *)bo, height);
    return height;
}
uint32_t gbm_bo_get_stride(struct gbm_bo *bo) {
    uint32_t stride = bo ? bo->stride : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_stride bo=%p stride=%u\n",
           (void *)bo, stride);
    return stride;
}
uint32_t gbm_bo_get_stride_for_plane(struct gbm_bo *bo, int plane) {
    uint32_t stride = bo && plane == 0 ? bo->stride : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_stride_for_plane bo=%p plane=%d "
           "stride=%u\n",
           (void *)bo, plane, stride);
    return stride;
}
uint32_t gbm_bo_get_offset(struct gbm_bo *bo, int plane) {
    uint32_t offset =
        bo && plane == 0 && bo->offset <= UINT32_MAX ? (uint32_t)bo->offset : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_offset bo=%p plane=%d offset=%u "
           "valid=%d\n",
           (void *)bo, plane, offset, bo && plane == 0);
    return offset;
}
uint32_t gbm_bo_get_format(struct gbm_bo *bo) {
    uint32_t format = bo ? bo->format : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_format bo=%p format=0x%08x\n",
           (void *)bo, format);
    return format;
}
uint64_t gbm_bo_get_modifier(struct gbm_bo *bo) {
    uint64_t modifier = bo ? bo->modifier : UINT64_MAX;
    debugf("[hl-gbm-shim] gbm_bo_get_modifier bo=%p modifier=0x%016llx\n",
           (void *)bo, (unsigned long long)modifier);
    return modifier;
}
int gbm_bo_get_plane_count(struct gbm_bo *bo) {
    int planes = bo ? 1 : 0;
    debugf("[hl-gbm-shim] gbm_bo_get_plane_count bo=%p planes=%d\n",
           (void *)bo, planes);
    return planes;
}
int gbm_bo_get_fd(struct gbm_bo *bo) {
    int fd = bo ? duplicate_fd(bo->fd) : -1;
    debugf("[hl-gbm-shim] gbm_bo_get_fd bo=%p fd=%d\n", (void *)bo, fd);
    return fd;
}
int gbm_bo_get_fd_for_plane(struct gbm_bo *bo, int plane) {
    int fd = bo && plane == 0 ? duplicate_fd(bo->fd) : -1;
    debugf("[hl-gbm-shim] gbm_bo_get_fd_for_plane bo=%p plane=%d fd=%d\n",
           (void *)bo, plane, fd);
    return fd;
}
union gbm_bo_handle gbm_bo_get_handle(struct gbm_bo *bo) {
    union gbm_bo_handle handle = {.u32 = bo ? (uint32_t)bo->fd : 0};
    debugf("[hl-gbm-shim] gbm_bo_get_handle bo=%p handle=%u\n",
           (void *)bo, handle.u32);
    return handle;
}
union gbm_bo_handle gbm_bo_get_handle_for_plane(struct gbm_bo *bo, int plane) {
    union gbm_bo_handle handle = {
        .u32 = bo && plane == 0 ? (uint32_t)bo->fd : 0
    };
    debugf("[hl-gbm-shim] gbm_bo_get_handle_for_plane bo=%p plane=%d "
           "handle=%u\n",
           (void *)bo, plane, handle.u32);
    return handle;
}

void *gbm_bo_map(struct gbm_bo *bo, uint32_t x, uint32_t y, uint32_t width,
                 uint32_t height, uint32_t flags, uint32_t *stride,
                 void **map_data) {
    debugf("[hl-gbm-shim] gbm_bo_map bo=%p x=%u y=%u width=%u height=%u "
           "flags=0x%08x\n",
           (void *)bo, x, y, width, height, flags);
    if (!bo || !width || !height || !stride || !map_data ||
        !(flags & (GBM_BO_TRANSFER_READ | GBM_BO_TRANSFER_WRITE)) ||
        (flags & ~(GBM_BO_TRANSFER_READ | GBM_BO_TRANSFER_WRITE)) ||
        x >= bo->width || y >= bo->height || width > bo->width - x ||
        height > bo->height - y) {
        errno = EINVAL;
        return NULL;
    }
    if (bo->modifier == HL_SURFACE_MODIFIER) {
        errno = ENOTSUP;
        return NULL;
    }
    int protection = 0;
    if (flags & GBM_BO_TRANSFER_READ) protection |= PROT_READ;
    if (flags & GBM_BO_TRANSFER_WRITE) protection |= PROT_WRITE;
    if (bo->offset > SIZE_MAX - bo->size) {
        errno = EOVERFLOW;
        return NULL;
    }
    size_t mapping_size = (size_t)bo->offset + bo->size;
    void *mapping =
        mmap(NULL, mapping_size, protection, MAP_SHARED, bo->fd, 0);
    if (mapping == MAP_FAILED) return NULL;
    *stride = bo->stride;
    *map_data = mapping;
    return (unsigned char *)mapping + (size_t)bo->offset +
           (size_t)y * bo->stride + (size_t)x * 4;
}

void gbm_bo_unmap(struct gbm_bo *bo, void *map_data) {
    debugf("[hl-gbm-shim] gbm_bo_unmap bo=%p map=%p\n", (void *)bo, map_data);
    if (bo && map_data && bo->offset <= SIZE_MAX - bo->size)
        munmap(map_data, (size_t)bo->offset + bo->size);
}

int gbm_bo_write(struct gbm_bo *bo, const void *buffer, size_t count) {
    debugf("[hl-gbm-shim] gbm_bo_write bo=%p count=%zu\n", (void *)bo, count);
    if (!bo || !buffer || count > bo->size) {
        errno = EINVAL;
        return -1;
    }
    if (bo->modifier == HL_SURFACE_MODIFIER) {
        errno = ENOTSUP;
        return -1;
    }
    return pwrite(bo->fd, buffer, count, (off_t)bo->offset) ==
                   (ssize_t)count
               ? 0
               : -1;
}

char *gbm_format_get_name(uint32_t format, void *name) {
    if (!name) return NULL;
    memcpy(name, &format, 4);
    ((char *)name)[4] = '\0';
    return name;
}
