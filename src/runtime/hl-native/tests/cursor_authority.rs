use std::{fs, path::PathBuf, process::Command};

#[test]
fn provider_cursor_owns_and_walks_mutable_handles() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-cursor-authority-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("cursor probe directory");
    let source = scratch.join("cursor_authority.c");
    let executable = scratch.join("cursor_authority");
    fs::write(
        &source,
        r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "hl/host_services.h"

#define HL_LINUX_VFS_LOWER_CAPACITY 1
#define HL_NFD 16

static int references[4];
static int clones;
static int closes;
static int child_present = 1;

static hl_host_result result(int32_t status, uint64_t value) {
    return (hl_host_result){.status = status, .value = value};
}

static hl_host_result clone_file(void *context, hl_host_handle handle) {
    (void)context;
    references[handle]++;
    clones++;
    return result(HL_STATUS_OK, handle);
}

static hl_host_result close_file(void *context, hl_host_handle handle) {
    (void)context;
    if (handle > 3 || references[handle] == 0) return result(HL_STATUS_INVALID_ARGUMENT, 0);
    references[handle]--;
    closes++;
    return result(HL_STATUS_OK, 0);
}

static hl_host_result open_relative(void *context, hl_host_handle directory, const char *path, size_t size,
                                    uint32_t access, uint32_t creation, uint32_t permissions) {
    (void)context; (void)creation; (void)permissions;
    if (directory != 1) return result(HL_STATUS_NOT_DIRECTORY, 0);
    hl_host_handle opened = 0;
    if (size == 1 && path[0] == '.') opened = 1;
    if (size == 3 && !memcmp(path, "dir", 3) && child_present) opened = 2;
    if (size == 4 && !memcmp(path, "link", 4)) opened = 3;
    if (size == 12 && !memcmp(path, ".wh.missing", 11)) opened = 0;
    if (opened == 0) return result(HL_STATUS_NOT_FOUND, 0);
    if ((access & HL_HOST_FILE_DIRECTORY) && opened != 1 && opened != 2)
        return result(HL_STATUS_NOT_DIRECTORY, 0);
    references[opened]++;
    return result(HL_STATUS_OK, opened);
}

static hl_host_result metadata(void *context, hl_host_handle handle, hl_host_file_metadata *output) {
    (void)context;
    memset(output, 0, sizeof *output);
    output->stable_device = 7;
    output->stable_object = handle;
    output->permissions = 0755;
    output->type = handle == 3 ? HL_HOST_FILE_TYPE_SYMLINK : HL_HOST_FILE_TYPE_DIRECTORY;
    return result(HL_STATUS_OK, 0);
}

static hl_host_result readlink_file(void *context, hl_host_handle handle, hl_host_bytes output) {
    (void)context;
    static const char target[] = "dir";
    if (handle != 3 || output.size < sizeof target - 1) return result(HL_STATUS_INVALID_ARGUMENT, 0);
    memcpy(output.data, target, sizeof target - 1);
    return result(HL_STATUS_OK, sizeof target - 1);
}

static const hl_host_file_services files = {
    .abi = HL_HOST_FILE_ABI, .size = sizeof files, .open_relative = open_relative, .metadata = metadata,
    .close = close_file, .readlink = readlink_file, .clone_for_fork = clone_file,
};
static const hl_host_services services = {
    .abi = HL_HOST_SERVICES_ABI, .size = sizeof services, .file = &files,
};

#include "linux_abi/container/vfs/cursor.c"

int main(void) {
    references[1] = 1;
    hl_vfs_cursor_authority root_authority = {
        .kind = HL_VFS_CURSOR_AUTHORITY_HOST,
        .value.host = {.handle = 1, .services = &services},
    };
    hl_vfs_cursor root;
    if (hl_vfs_cursor_root_authorities(&root_authority, NULL, 0, &root) != 0 || clones != 1) return 1;
    hl_vfs_cursor_entry entry;
    if (hl_vfs_cursor_lookup(&root, "dir", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY) return 2;
    hl_vfs_cursor_entry_release(&entry);
    if (hl_vfs_cursor_lookup(&root, "link", &entry) != 0 || entry.kind != HL_VFS_CURSOR_SYMLINK ||
        strcmp(entry.symlink, "dir")) return 3;
    hl_vfs_cursor_entry_release(&entry);
    child_present = 0;
    if (hl_vfs_cursor_lookup(&root, "dir", &entry) != -ENOENT) return 4;
    hl_vfs_cursor clone;
    if (hl_vfs_cursor_clone(&root, &clone) != 0 || clones < 3) return 5;
    hl_vfs_cursor_release(&clone);
    if (hl_vfs_fd_cursor_publish(4, &root) != 0) return 6;
    int before_fork_clones = clones;
    hl_vfs_fd_cursor_after_fork();
    if (hl_vfs_fd_cursor_get(4) == NULL || clones <= before_fork_clones) return 7;
    hl_vfs_fd_cursor_clear();
    if (hl_vfs_fd_cursor_get(4) != NULL) return 8;
    hl_vfs_cursor_release(&root);
    if (references[1] != 1 || closes == 0) return 9;
    return 0;
}
"#,
    )
    .expect("cursor probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-Wno-unused-function"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("cursor probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("cursor probe execution");
    assert!(run.success(), "cursor probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove cursor probe directory");
}
