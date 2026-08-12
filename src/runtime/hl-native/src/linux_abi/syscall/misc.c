#include "misc.h"

#include <errno.h>
#include <string.h>

static void uname_field(char destination[65], const char *source) {
    size_t length = strlen(source);
    if (length > 64u) length = 64u;
    memcpy(destination, source, length);
    destination[length] = 0;
}

int hl_linux_misc_dispatch(hl_linux_misc_context *context, uint64_t number, const uint64_t arguments[6],
                           int64_t *guest_result) {
    uint64_t address = arguments[0];
    uint64_t size = arguments[1];
    switch (number) {
    case 160: {
        char output[6 * 65] = {0};
        uname_field(output, "Linux");
        uname_field(output + 65, context->hostname[0] ? context->hostname : "jit");
        uname_field(output + 130, "6.1.0");
        uname_field(output + 195, "#1 jit");
        uname_field(output + 260, context->machine);
        if (context->copy_to(context->callback_context, address, output, sizeof output) != sizeof output) {
            *guest_result = -EFAULT;
            break;
        }
        *guest_result = 0;
        break;
    }
    case 161: {
        int length = (int)size;
        if (context->hostname == NULL || context->hostname_capacity == 0) {
            *guest_result = -EINVAL;
            break;
        }
        if (length > 64) length = 64;
        if (length > 0) {
            char hostname[64];
            if (context->copy_from(context->callback_context, hostname, address, (size_t)length) != length) {
                *guest_result = -EFAULT;
                break;
            }
            memcpy(context->hostname, hostname, (size_t)length);
            context->hostname[length < (int)context->hostname_capacity ? length : (int)context->hostname_capacity - 1] =
                0;
        }
        *guest_result = 0;
        break;
    }
    case 162: *guest_result = 0; break;
    case 179: {
        unsigned char output[112] = {0};
        uint64_t total;
        uint64_t free_memory;
        // totalram MUST agree with /proc/meminfo MemTotal and /sys/fs/cgroup/memory.max: a cgroup memory cap
        // wins; otherwise report the host RAM total (the same figure vfs.c serves to MemTotal). The old
        // hardcoded 8 GiB disagreed with /proc/meminfo whenever the container was unconstrained, so a runtime
        // that sizes its heap off sysinfo (glibc get_phys_pages, some JVMs) and one that reads /proc/meminfo
        // saw two different machine sizes.
        total = context->memory_limit ? context->memory_limit
                                      : (context->host_memory_total ? context->host_memory_total : UINT64_C(8) << 30);
        if (context->memory_limit)
            free_memory = total > context->memory_used ? total - context->memory_used : 0;
        else
            free_memory = context->host_memory_free ? context->host_memory_free : total / 4;
        // uptime is monotonic seconds since boot (matches /proc/uptime); the old constant 3600 never advanced
        // and disagreed with /proc/uptime.
        uint64_t uptime = context->uptime_seconds ? context->uptime_seconds : 3600;
        uint32_t procs = context->process_count ? context->process_count : 1;
        memcpy(output + 0, &uptime, sizeof(uptime));
        memcpy(output + 8, &context->loads[0], sizeof(uint64_t));
        memcpy(output + 16, &context->loads[1], sizeof(uint64_t));
        memcpy(output + 24, &context->loads[2], sizeof(uint64_t));
        memcpy(output + 32, &total, sizeof(total));
        memcpy(output + 40, &free_memory, sizeof(free_memory));
        memcpy(output + 80, &procs, sizeof(uint16_t));
        memcpy(output + 104, &(uint32_t){1}, sizeof(uint32_t));
        if (context->copy_to(context->callback_context, address, output, sizeof output) != sizeof output) {
            *guest_result = -EFAULT;
            break;
        }
        *guest_result = 0;
        break;
    }
    case 278: {
        // Validate flags exactly as Linux (drivers/char/random.c): only GRND_NONBLOCK(1) | GRND_RANDOM(2) |
        // GRND_INSECURE(4) are defined, and GRND_RANDOM|GRND_INSECURE together is invalid. Any other bit ->
        // EINVAL (previously an unknown flag such as 0x10 wrongly succeeded).
        uint64_t flags = arguments[2];
        if ((flags & ~(uint64_t)0x7u) || (flags & 0x2u && flags & 0x4u)) {
            *guest_result = -EINVAL;
            break;
        }
        unsigned char random[4096];
        uint64_t done = 0;
        while (done < size) {
            size_t chunk = size - done < sizeof random ? (size_t)(size - done) : sizeof random;
            context->random(context->callback_context, random, chunk);
            ssize_t copied = context->copy_to(context->callback_context, address + done, random, chunk);
            if (copied != (ssize_t)chunk) {
                *guest_result = done || copied > 0 ? (int64_t)(done + (copied > 0 ? (uint64_t)copied : 0)) : -EFAULT;
                break;
            }
            done += chunk;
        }
        if (done == size) *guest_result = (int64_t)done;
        break;
    }
    case 293: *guest_result = -ENOSYS; break;
    default: return 0;
    }
    return 1;
}
