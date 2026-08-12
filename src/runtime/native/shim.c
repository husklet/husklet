#include "core/engine_backend.h"
#include "core/options.h"
#include "core/provider/client.h"
#include "core/provider/tree_files.h"
#include "executable_authority.h"
#include "hl/engine.h"
#include "hl/linux.h"
#include "hl/linux_abi.h"
#include "hl/syscall_trap.h"
#include "main_plan.h"

#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Explicitly anchors the lifecycle object when the retained backend is linked
 * from static archives. The function is idempotent and replaces reliance on
 * linker-specific constructor extraction. */
extern void hl_aarch64_target_register_backend(void);
extern void hl_x86_64_target_register_backend(void);

typedef struct hl_c_backend {
  hl_host_linux *host;
  hl_host_services services;
  hl_engine *engine;
  hl_options options;
  uint32_t options_initialized;
  hl_provider_client provider;
  uint32_t provider_initialized;
  uint32_t provider_files_installed;
  int32_t provider_fd;
  hl_engine_exit result;
} hl_c_backend;

static void hl_c_backend_provider_discard(hl_c_backend *backend) {
  if (backend == NULL || !backend->provider_initialized)
    return;
  if (backend->provider_files_installed) {
    hl_provider_tree_files_revoke();
    backend->provider_files_installed = 0;
  }
  hl_provider_client_destroy(&backend->provider);
  close(backend->provider_fd);
  backend->provider_fd = -1;
  backend->provider_initialized = 0;
}

static uint32_t hl_c_backend_status_flags(uint64_t detail) {
  uint32_t flags;
  const uint64_t access = detail & (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE);
  if (access == (HL_HOST_FILE_READ | HL_HOST_FILE_WRITE))
    flags = HL_LINUX_O_RDWR;
  else if (access == HL_HOST_FILE_WRITE)
    flags = HL_LINUX_O_WRONLY;
  else
    flags = HL_LINUX_O_RDONLY;
  if ((detail & HL_HOST_FILE_APPEND) != 0)
    flags |= HL_LINUX_O_APPEND;
  if ((detail & HL_HOST_FILE_NONBLOCK) != 0)
    flags |= HL_LINUX_O_NONBLOCK;
  return flags;
}

static int hl_c_validate_main_image_plan(int fd,
                                         const hl_c_main_image_plan *plan) {
  uint8_t header[64];
  if (plan == NULL || plan->abi != HL_C_MAIN_IMAGE_PLAN_ABI ||
      plan->size < sizeof(*plan) || plan->architecture == 0 ||
      plan->reserved != 0 || plan->link_end <= plan->link_start)
    return 0;
  if (pread(fd, header, sizeof(header), 0) != (ssize_t)sizeof(header))
    return 0;
  if (memcmp(header, "\177ELF", 4) != 0 || header[4] != 2 || header[5] != 1)
    return 0;
  uint16_t type, machine;
  memcpy(&type, header + 16, sizeof(type));
  memcpy(&machine, header + 18, sizeof(machine));
  uint32_t kind = type == 2   ? HL_C_IMAGE_EXECUTABLE
                  : type == 3 ? HL_C_IMAGE_POSITION_INDEPENDENT
                              : 0;
  uint16_t expected_machine = plan->architecture == 1   ? 0xb7
                              : plan->architecture == 2 ? 0x3e
                                                        : 0;
  uint64_t phoff;
  uint16_t phentsize, phnum;
  memcpy(&phoff, header + 32, sizeof(phoff));
  memcpy(&phentsize, header + 54, sizeof(phentsize));
  memcpy(&phnum, header + 56, sizeof(phnum));
  if (kind != plan->kind || machine != expected_machine || phentsize < 56 ||
      phnum == 0)
    return 0;
  uint64_t first = UINT64_MAX, last = 0;
  uint32_t has_interpreter = 0;
  uint64_t interpreter_identity = 0;
  for (uint16_t index = 0; index < phnum; ++index) {
    uint8_t ph[56];
    uint64_t offset = phoff + (uint64_t)index * phentsize;
    if (offset < phoff ||
        pread(fd, ph, sizeof(ph), (off_t)offset) != (ssize_t)sizeof(ph))
      return 0;
    uint32_t ph_type;
    memcpy(&ph_type, ph, sizeof(ph_type));
    if (ph_type == 3) {
      uint64_t file_offset, file_size;
      memcpy(&file_offset, ph + 8, sizeof(file_offset));
      memcpy(&file_size, ph + 32, sizeof(file_size));
      if (file_size == 0 || file_size > 4096)
        return 0;
      uint8_t interpreter[4096];
      if (pread(fd, interpreter, (size_t)file_size, (off_t)file_offset) !=
          (ssize_t)file_size)
        return 0;
      size_t length = (size_t)file_size;
      if (length != 0 && interpreter[length - 1] == 0)
        length--;
      interpreter_identity = UINT64_C(0xcbf29ce484222325);
      for (size_t byte = 0; byte < length; ++byte)
        interpreter_identity = (interpreter_identity ^ interpreter[byte]) *
                               UINT64_C(0x100000001b3);
      has_interpreter = 1;
    }
    if (ph_type != 1)
      continue;
    uint64_t address, size;
    memcpy(&address, ph + 16, sizeof(address));
    memcpy(&size, ph + 40, sizeof(size));
    if (address + size < address)
      return 0;
    if (address < first)
      first = address;
    if (address + size > last)
      last = address + size;
  }
  if (first == UINT64_MAX)
    return 0;
  uint64_t start = first & ~UINT64_C(0xfff);
  if (last < start || last - start > UINT64_MAX - UINT64_C(0xffff))
    return 0;
  uint64_t end =
      start + ((last - start + UINT64_C(0xffff)) & ~UINT64_C(0xffff));
  return start == plan->link_start && end == plan->link_end &&
         has_interpreter == plan->has_interpreter &&
         interpreter_identity == plan->interpreter_identity;
}

int32_t hl_c_backend_create(
    uint32_t isa, const char *rootfs, const char *executable_host,
    int32_t executable_fd, const hl_c_main_image_plan *image_plan,
    uint32_t option_count, const char *const *option_names,
    const char *const *option_values, const int32_t standard_fds[3],
    int32_t provider_fd, void *syscall_context,
    hl_syscall_trap_fn syscall_dispatch, hl_c_backend **output) {
  hl_c_backend *backend;
  hl_engine_config config;
  hl_status status;
  uint32_t index;
  hl_engine_fd_binding bindings[3];
  hl_host_result imported[3];
  hl_engine_executable executable;
  if (output == NULL) {
    if (provider_fd >= 0)
      close(provider_fd);
    return HL_STATUS_INVALID_ARGUMENT;
  }
  int validation_fd = executable_fd;
  int validation_owned = 0;
  if (validation_fd < 0 && executable_host != NULL) {
    validation_fd = open(executable_host, O_RDONLY | O_CLOEXEC);
    validation_owned = validation_fd >= 0;
  }
  int validation_ok = validation_fd >= 0 &&
                      hl_c_validate_main_image_plan(validation_fd, image_plan);
  if (validation_owned)
    close(validation_fd);
  if (!validation_ok) {
    if (provider_fd >= 0)
      close(provider_fd);
    return HL_STATUS_INVALID_ARGUMENT;
  }
  *output = NULL;
  backend = calloc(1, sizeof(*backend));
  if (backend == NULL) {
    if (provider_fd >= 0)
      close(provider_fd);
    return HL_STATUS_OUT_OF_MEMORY;
  }
  status = hl_host_linux_create(&backend->host, &backend->services);
  if (status != HL_STATUS_OK) {
    if (provider_fd >= 0)
      close(provider_fd);
    free(backend);
    return status;
  }
  backend->provider_fd = -1;
  if (provider_fd >= 0) {
    if (provider_fd < 3 ||
        (standard_fds != NULL &&
         (provider_fd == standard_fds[0] || provider_fd == standard_fds[1] ||
          provider_fd == standard_fds[2]))) {
      hl_host_linux_destroy(backend->host);
      close(provider_fd);
      free(backend);
      return HL_STATUS_INVALID_ARGUMENT;
    }
    backend->provider_initialized = 1;
    backend->provider_fd = provider_fd;
    if (hl_provider_client_init(&backend->provider, provider_fd, 4096) != 0) {
      backend->provider_initialized = 0;
      hl_host_linux_destroy(backend->host);
      close(provider_fd);
      free(backend);
      return HL_STATUS_INVALID_ARGUMENT;
    }
    if (hl_provider_tree_files_install(&backend->services,
                                       &backend->provider) != 0) {
      hl_c_backend_provider_discard(backend);
      hl_host_linux_destroy(backend->host);
      free(backend);
      return HL_STATUS_INVALID_ARGUMENT;
    }
    backend->provider_files_installed = 1;
  }
  memset(&config, 0, sizeof(config));
  hl_aarch64_target_register_backend();
  hl_x86_64_target_register_backend();
  config.abi = HL_ENGINE_ABI;
  config.size = sizeof(config);
  config.guest_isa = isa;
  config.rootfs = rootfs;
  memset(bindings, 0, sizeof(bindings));
  memset(imported, 0, sizeof(imported));
  memset(&executable, 0, sizeof(executable));
  if (standard_fds != NULL) {
    for (index = 0; index < 3; ++index) {
      imported[index] =
          hl_host_linux_import_file(backend->host, standard_fds[index]);
      if (imported[index].status != HL_STATUS_OK) {
        uint32_t close_index;
        for (close_index = 0; close_index < index; ++close_index)
          (void)backend->services.file->close(backend->services.context,
                                              imported[close_index].value);
        hl_c_backend_provider_discard(backend);
        hl_host_linux_destroy(backend->host);
        free(backend);
        return imported[index].status;
      }
      bindings[index].abi = HL_ENGINE_ABI;
      bindings[index].size = sizeof(bindings[index]);
      bindings[index].guest_fd = index;
      bindings[index].status_flags =
          hl_c_backend_status_flags(imported[index].detail);
      bindings[index].ownership = HL_ENGINE_FD_TRANSFER;
      bindings[index].host_handle = imported[index].value;
    }
    config.fd_bindings = bindings;
    config.fd_binding_count = 3;
  }
  if (hl_options_init_records(&backend->options, option_count, option_names,
                              option_values) != 0) {
    hl_c_backend_provider_discard(backend);
    hl_host_linux_destroy(backend->host);
    free(backend);
    return HL_STATUS_INVALID_ARGUMENT;
  }
  backend->options_initialized = 1;
  config.main_image_plan = image_plan;
  if (executable_fd >= 0) {
    hl_host_result imported_executable =
        hl_host_linux_import_file(backend->host, executable_fd);
    if (imported_executable.status != HL_STATUS_OK ||
        imported_executable.value == HL_HOST_HANDLE_INVALID) {
      hl_options_destroy(&backend->options);
      if (standard_fds != NULL)
        for (index = 0; index < 3; ++index)
          (void)backend->services.file->close(backend->services.context,
                                              imported[index].value);
      hl_c_backend_provider_discard(backend);
      hl_host_linux_destroy(backend->host);
      free(backend);
      return imported_executable.status == HL_STATUS_OK
                 ? HL_STATUS_PLATFORM_FAILURE
                 : imported_executable.status;
    }
    executable = (hl_engine_executable){
        .abi = HL_ENGINE_ABI,
        .size = sizeof(executable),
        .ownership = HL_ENGINE_FD_TRANSFER,
        .reserved = 0,
        .host_handle = imported_executable.value,
        .image = NULL,
        .image_size = 0,
    };
    config.executable = &executable;
  } else if (executable_host != NULL) {
    status = hl_c_backend_executable_open(&backend->services, executable_host,
                                          &executable);
    if (status != HL_STATUS_OK) {
      hl_options_destroy(&backend->options);
      hl_c_backend_provider_discard(backend);
      hl_host_linux_destroy(backend->host);
      free(backend);
      return status;
    }
    config.executable = &executable;
  }
  status = hl_engine_create_with_borrowed_options_and_syscall_trap(
      &config, &backend->services, &backend->options, syscall_context,
      syscall_dispatch, &backend->engine);
  if (status != HL_STATUS_OK) {
    hl_c_backend_executable_discard(&backend->services, &executable);
    if (standard_fds != NULL)
      for (index = 0; index < 3; ++index)
        (void)backend->services.file->close(backend->services.context,
                                            imported[index].value);
    hl_c_backend_provider_discard(backend);
    hl_host_linux_destroy(backend->host);
    hl_options_destroy(&backend->options);
    free(backend);
    return status;
  }
  backend->result.abi = HL_ENGINE_ABI;
  backend->result.size = sizeof(backend->result);
  *output = backend;
  return HL_STATUS_OK;
}

int32_t hl_c_backend_run(hl_c_backend *backend, int32_t argc,
                         const char *const *argv) {
  if (backend == NULL)
    return HL_STATUS_INVALID_ARGUMENT;
  return hl_engine_run(backend->engine, argc, argv, &backend->result);
}

int32_t hl_c_backend_request(hl_c_backend *backend, uint32_t request,
                             int32_t signal) {
  if (backend == NULL)
    return HL_STATUS_INVALID_ARGUMENT;
  if (request == HL_ENGINE_REQUEST_SIGNAL)
    return hl_engine_request(backend->engine, request, &signal, sizeof(signal));
  return hl_engine_request(backend->engine, request, NULL, 0);
}

uint32_t hl_c_backend_exit_kind(const hl_c_backend *backend) {
  return backend == NULL ? 0 : backend->result.kind;
}
int32_t hl_c_backend_exit_status(const hl_c_backend *backend) {
  return backend == NULL ? -1 : backend->result.guest_status;
}
uint64_t hl_c_backend_exit_detail(const hl_c_backend *backend) {
  return backend == NULL ? 0 : backend->result.detail;
}

void hl_c_backend_destroy(hl_c_backend *backend) {
  if (backend == NULL)
    return;
  hl_engine_destroy(backend->engine);
  if (backend->options_initialized)
    hl_options_destroy(&backend->options);
  hl_c_backend_provider_discard(backend);
  hl_host_linux_destroy(backend->host);
  free(backend);
}
