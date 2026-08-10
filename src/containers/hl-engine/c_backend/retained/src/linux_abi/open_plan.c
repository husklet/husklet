#include "open_plan.h"

#include <string.h>

static hl_open_synthetic synthetic_kind(const char *path) {
    if (strcmp(path, "/proc") == 0 || strncmp(path, "/proc/", 6) == 0) return HL_OPEN_SYNTHETIC_PROC;
    if (strcmp(path, "/sys") == 0 || strncmp(path, "/sys/", 5) == 0) return HL_OPEN_SYNTHETIC_SYS;
    if (strcmp(path, "/dev") == 0 || strncmp(path, "/dev/", 5) == 0) return HL_OPEN_SYNTHETIC_DEV;
    return HL_OPEN_SYNTHETIC_NONE;
}

static hl_status normalize_absolute(const char *input, size_t size, char output[4097], size_t *output_size) {
    size_t starts[512];
    size_t lengths[512];
    size_t count = 0;
    size_t index = 0;
    size_t used = 0;
    while (index < size) {
        size_t start;
        size_t length;
        while (index < size && input[index] == '/')
            index++;
        start = index;
        while (index < size && input[index] != '/')
            index++;
        length = index - start;
        if (length == 0 || (length == 1 && input[start] == '.')) continue;
        if (length == 2 && input[start] == '.' && input[start + 1] == '.') {
            if (count != 0) count--;
            continue;
        }
        if (count == 512) return HL_STATUS_RESOURCE_LIMIT;
        starts[count] = start;
        lengths[count] = length;
        count++;
    }
    output[used++] = '/';
    for (index = 0; index < count; ++index) {
        if (used != 1) output[used++] = '/';
        if (lengths[index] > 4096 - used) return HL_STATUS_RESOURCE_LIMIT;
        memcpy(output + used, input + starts[index], lengths[index]);
        used += lengths[index];
    }
    output[used] = '\0';
    *output_size = used;
    return HL_STATUS_OK;
}

hl_status hl_open_plan_build(const hl_open_request *request, hl_open_plan *plan) {
    hl_status status;
    int writing;
    if (request == NULL || plan == NULL || request->guest_path == NULL || request->guest_path_size == 0 ||
        request->guest_path_size > 4096)
        return HL_STATUS_INVALID_ARGUMENT;
    memset(plan, 0, sizeof(*plan));
    plan->directory = request->directory;
    plan->intent = request->intent;
    if (request->guest_path[0] == '/') {
        status = normalize_absolute(request->guest_path, request->guest_path_size, plan->path, &plan->path_size);
        if (status != HL_STATUS_OK) return status;
    } else {
        memcpy(plan->path, request->guest_path, request->guest_path_size);
        plan->path[request->guest_path_size] = '\0';
        plan->path_size = request->guest_path_size;
    }
    writing = (request->intent &
               (HL_OPEN_WRITE | HL_OPEN_CREATE | HL_OPEN_TRUNCATE | HL_OPEN_APPEND | HL_OPEN_TEMPORARY)) != 0;
    if (request->read_only && writing) {
        plan->kind = HL_OPEN_ERROR;
        plan->error = HL_STATUS_PERMISSION_DENIED;
        return HL_STATUS_OK;
    }
    if ((request->intent & HL_OPEN_TEMPORARY) != 0) {
        plan->kind = HL_OPEN_TMPFILE;
        return HL_STATUS_OK;
    }
    plan->synthetic = synthetic_kind(plan->path);
    if (plan->synthetic != HL_OPEN_SYNTHETIC_NONE) {
        plan->kind = HL_OPEN_SYNTHETIC;
        return HL_STATUS_OK;
    }
    if (request->overlay) {
        plan->kind = HL_OPEN_OVERLAY;
        plan->overlay = (request->intent & HL_OPEN_CREATE) != 0 ? HL_OPEN_OVERLAY_CREATE
                        : writing                               ? HL_OPEN_OVERLAY_COPY_UP
                                                                : HL_OPEN_OVERLAY_LOOKUP;
    } else {
        plan->kind = HL_OPEN_HOST_PATH;
    }
    plan->names_symlink = request->final_symlink && (request->intent & (HL_OPEN_PATH_ONLY | HL_OPEN_NOFOLLOW)) ==
                                                        (HL_OPEN_PATH_ONLY | HL_OPEN_NOFOLLOW);
    return HL_STATUS_OK;
}
