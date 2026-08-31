use std::{
    fs,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};

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
static int clone_fails;
static int context_poisoned;
static unsigned child_permissions = 0755;

static hl_host_result result(int32_t status, uint64_t value) {
    return (hl_host_result){.status = status, .value = value};
}

static hl_host_result clone_file(void *context, hl_host_handle handle) {
    (void)context;
    if (context_poisoned) abort();
    if (clone_fails) return result(HL_STATUS_OUT_OF_MEMORY, 0);
    references[handle]++;
    clones++;
    return result(HL_STATUS_OK, handle);
}

static hl_host_result close_file(void *context, hl_host_handle handle) {
    (void)context;
    if (context_poisoned) abort();
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
    output->permissions = handle == 2 ? child_permissions : 0755;
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

/* This fixture compiles the cursor without the container namespace that owns bind mounts, so the
 * mount edge has no volume table to consult and every guest path stays inside the merged layers. */
static int hl_vfs_cursor_mount_authority(const char *guest, hl_vfs_cursor_authority *output) {
    (void)guest;
    (void)output;
    return 0;
}

static int search_hook(const hl_vfs_cursor *directory, void *context) {
    int *calls = context;
    (*calls)++;
    if (directory->count != 0 && directory->layers[0].kind == HL_VFS_CURSOR_AUTHORITY_HOST) {
        const hl_vfs_cursor_authority *authority = &directory->layers[0];
        hl_host_file_metadata status;
        if (authority->value.host.services->file->metadata(authority->value.host.services->context,
                                                           authority->value.host.handle, &status).status != HL_STATUS_OK)
            return -EIO;
        if ((status.permissions & 0111) == 0) return -EACCES;
    }
    return 0;
}

static int terminal_hook(const char *guest, void *context) {
    (void)context;
    return !strcmp(guest, "/dir/missing");
}

static int terminal_denied(const char *guest, void *context) {
    (void)context;
    return !strcmp(guest, "/dir/missing") ? -EACCES : 0;
}

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
    child_present = 1;
    child_permissions = 0;
    int search_calls = 0;
    if (hl_vfs_cursor_walk(&root, &root, "link/missing", 0, 1, 0, NULL, 0, NULL, NULL, NULL, search_hook, &search_calls, &entry) != -EACCES ||
        search_calls < 2) return 12;
    search_calls = 0;
    if (hl_vfs_cursor_walk(&root, &root, "dir/", 0, 1, 0, NULL, 0, NULL, NULL, NULL, search_hook, &search_calls, &entry) != -EACCES ||
        search_calls < 2) return 13;
    search_calls = 0;
    char denied_terminal[16] = "uninitialized";
    int denied_requires_directory = 7;
    if (hl_vfs_cursor_walk(&root, &root, "dir/missing", 0, 1, 1, denied_terminal, sizeof denied_terminal,
                           &denied_requires_directory, terminal_hook, NULL, search_hook, &search_calls, &entry) !=
            -EACCES ||
        search_calls < 2 || denied_terminal[0] != 0 || denied_requires_directory != 0) return 20;
    child_permissions = 0755;
    search_calls = 0;
    if (hl_vfs_cursor_walk(&root, &root, "dir/missing", 0, 1, 1, denied_terminal, sizeof denied_terminal,
                           &denied_requires_directory, terminal_denied, NULL, search_hook, &search_calls, &entry) !=
            -EACCES ||
        search_calls < 2 || denied_terminal[0] != 0 || denied_requires_directory != 0) return 22;
    for (size_t index = 0; index < 3; ++index) {
        const char *no_final[] = {"/", ".", ".."};
        char resolved[16] = "uninitialized";
        int requires_directory = 7;
        if (hl_vfs_cursor_walk(&root, &root, no_final[index], 0, 1, 1, resolved, sizeof resolved,
                               &requires_directory, NULL, NULL, search_hook, &search_calls, &entry) != HL_VFS_CURSOR_NO_FINAL ||
            resolved[0] != 0 || requires_directory != 0) return 14 + (int)index;
    }
    char resolved_link[16];
    int requires_directory = 0;
    if (hl_vfs_cursor_walk(&root, &root, "link", 0, 1, 1, resolved_link, sizeof resolved_link,
                           &requires_directory, NULL, NULL, search_hook, &search_calls, &entry) != 0 ||
        strcmp(resolved_link, "/dir") || requires_directory) return 17;
    if (hl_vfs_cursor_walk(&root, &root, "link", 1, 1, 1, resolved_link, sizeof resolved_link,
                           &requires_directory, NULL, NULL, search_hook, &search_calls, &entry) != 0 ||
        strcmp(resolved_link, "/link") || requires_directory) return 18;
    if (hl_vfs_cursor_walk(&root, &root, "link/", 1, 1, 1, resolved_link, sizeof resolved_link,
                           &requires_directory, NULL, NULL, search_hook, &search_calls, &entry) != 0 ||
        strcmp(resolved_link, "/dir") || !requires_directory) return 21;
    if (hl_vfs_cursor_walk(&root, &root, "link/missing", 0, 1, 1, resolved_link, sizeof resolved_link,
                           &requires_directory, terminal_hook, NULL, search_hook, &search_calls, &entry) != 0 ||
        strcmp(resolved_link, "/dir/missing") || requires_directory) return 19;
    hl_vfs_cursor clone;
    if (hl_vfs_cursor_clone(&root, &clone) != 0 || clones < 3) return 5;
    hl_vfs_cursor_release(&clone);
    if (hl_vfs_fd_cursor_publish(4, &root) != 0) return 6;
    int before_fork_clones = clones;
    int before_failure_references = references[1];
    clone_fails = 1;
    hl_vfs_cursor **failed = calloc(HL_NFD, sizeof *failed);
    if (hl_vfs_fd_cursor_clone_table(failed) == 0 || hl_vfs_fd_cursor_get(4) == NULL ||
        references[1] != before_failure_references) return 7;
    hl_vfs_fd_cursor_release_table(failed);
    free(failed);
    clone_fails = 0;
    hl_vfs_cursor **forked = calloc(HL_NFD, sizeof *forked);
    if (hl_vfs_fd_cursor_clone_table(forked) != 0) return 8;
    hl_vfs_fd_cursor_replace_table(forked);
    free(forked);
    if (hl_vfs_fd_cursor_get(4) == NULL || clones <= before_fork_clones) return 9;
    hl_vfs_fd_cursor_clear();
    if (hl_vfs_fd_cursor_get(4) != NULL) return 10;
    context_poisoned = 1;
    /* A subsequent run begins only after teardown emptied the table; it must never call the poisoned old host. */
    hl_vfs_fd_cursor_clear();
    context_poisoned = 0;
    hl_vfs_cursor_release(&root);
    if (references[1] != 1 || closes == 0) return 11;
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

/// Overlay layer semantics the cursor walk owns: a `.wh.NAME` whiteout in a higher layer hides NAME in
/// every lower layer, a `.wh..wh..opq` marker cuts the lower layers out of a merged directory, and an
/// entry present only in a lower layer is still found. The walk skips both marker probes when there is
/// no layer below the one being examined -- this test pins that the skip never reaches a case where a
/// layer below exists, and that the probe descriptions the walk retains are all handed back.
#[test]
fn layer_markers_decide_visibility_and_hand_back_every_handle() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-cursor-layers-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("layer probe directory");
    let source = scratch.join("cursor_layers.c");
    let executable = scratch.join("cursor_layers");
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
#define HANDLES 32

/* Two layers. 1 = the writable UPPER root, 2 = the read-only LOWER root. Entries are a flat
   (parent, name) -> (handle, type) table so the fixture spells the on-disk overlay wire names
   (`.wh.NAME`, `.wh..wh..opq`) exactly as the real image layers carry them. */
struct entry { hl_host_handle parent; const char *name; hl_host_handle child; int type; };
static const struct entry entries[] = {
    /* upper */
    {1, "kept",          10, HL_HOST_FILE_TYPE_DIRECTORY},
    {1, ".wh.gone",      11, HL_HOST_FILE_TYPE_REGULAR},   /* whiteout hiding the lower "gone" */
    {1, "opq",           12, HL_HOST_FILE_TYPE_DIRECTORY},
    {12, ".wh..wh..opq", 13, HL_HOST_FILE_TYPE_REGULAR},   /* opaque marker on the upper "opq" */
    {1, "shadow",        14, HL_HOST_FILE_TYPE_DIRECTORY},
    /* lower */
    {2, "gone",          20, HL_HOST_FILE_TYPE_DIRECTORY},
    {2, "onlylower",     21, HL_HOST_FILE_TYPE_DIRECTORY},
    {2, "opq",           22, HL_HOST_FILE_TYPE_DIRECTORY},
    {2, "shadow",        23, HL_HOST_FILE_TYPE_DIRECTORY},
};

static int references[HANDLES];

static hl_host_result result(int32_t status, uint64_t value) {
    return (hl_host_result){.status = status, .value = value};
}

static hl_host_result clone_file(void *context, hl_host_handle handle) {
    (void)context;
    if (handle >= HANDLES) return result(HL_STATUS_INVALID_ARGUMENT, 0);
    references[handle]++;
    return result(HL_STATUS_OK, handle);
}

static hl_host_result close_file(void *context, hl_host_handle handle) {
    (void)context;
    if (handle >= HANDLES || references[handle] == 0) return result(HL_STATUS_INVALID_ARGUMENT, 0);
    references[handle]--;
    return result(HL_STATUS_OK, 0);
}

static int entry_type(hl_host_handle handle) {
    for (size_t index = 0; index < sizeof entries / sizeof entries[0]; index++)
        if (entries[index].child == handle) return entries[index].type;
    return HL_HOST_FILE_TYPE_DIRECTORY; /* the two roots */
}

static hl_host_result open_relative(void *context, hl_host_handle directory, const char *path, size_t size,
                                    uint32_t access, uint32_t creation, uint32_t permissions) {
    (void)context; (void)creation; (void)permissions;
    if (size == 1 && path[0] == '.') {
        references[directory]++;
        return result(HL_STATUS_OK, directory);
    }
    for (size_t index = 0; index < sizeof entries / sizeof entries[0]; index++) {
        if (entries[index].parent != directory) continue;
        if (strlen(entries[index].name) != size || memcmp(entries[index].name, path, size)) continue;
        if ((access & HL_HOST_FILE_DIRECTORY) && entries[index].type != HL_HOST_FILE_TYPE_DIRECTORY)
            return result(HL_STATUS_NOT_DIRECTORY, 0);
        references[entries[index].child]++;
        return result(HL_STATUS_OK, entries[index].child);
    }
    return result(HL_STATUS_NOT_FOUND, 0);
}

static hl_host_result metadata(void *context, hl_host_handle handle, hl_host_file_metadata *output) {
    (void)context;
    memset(output, 0, sizeof *output);
    output->stable_device = 7;
    output->stable_object = handle;
    output->permissions = 0755;
    output->type = entry_type(handle);
    return result(HL_STATUS_OK, 0);
}

static const hl_host_file_services files = {
    .abi = HL_HOST_FILE_ABI, .size = sizeof files, .open_relative = open_relative, .metadata = metadata,
    .close = close_file, .clone_for_fork = clone_file,
};
static const hl_host_services services = {
    .abi = HL_HOST_SERVICES_ABI, .size = sizeof services, .file = &files,
};

#include "linux_abi/container/vfs/cursor.c"

/* This fixture compiles the cursor without the container namespace that owns bind mounts, so the
 * mount edge has no volume table to consult and every guest path stays inside the merged layers. */
static int hl_vfs_cursor_mount_authority(const char *guest, hl_vfs_cursor_authority *output) {
    (void)guest;
    (void)output;
    return 0;
}

static hl_vfs_cursor_authority authority_for(hl_host_handle handle) {
    references[handle]++;
    hl_vfs_cursor_authority authority = {
        .kind = HL_VFS_CURSOR_AUTHORITY_HOST,
        .value.host = {.handle = handle, .services = &services},
    };
    return authority;
}

static int leaked(void) {
    for (size_t handle = 0; handle < HANDLES; handle++)
        if (references[handle] != 0) return (int)handle;
    return -1;
}

int main(void) {
    hl_vfs_cursor_authority upper = authority_for(1);
    hl_vfs_cursor_authority lower = authority_for(2);
    hl_vfs_cursor root;
    if (hl_vfs_cursor_root_authorities(&upper, &lower, 1, &root) != 0 || root.count != 2) return 1;
    hl_vfs_cursor_authority_close(&upper);
    hl_vfs_cursor_authority_close(&lower);

    hl_vfs_cursor_entry entry;
    /* A lower-only entry is still visible through the union. */
    if (hl_vfs_cursor_lookup(&root, "onlylower", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY ||
        entry.directory.count != 1 || entry.directory.layers[0].value.host.handle != 21) return 2;
    hl_vfs_cursor_entry_release(&entry);

    /* `.wh.gone` in the upper hides the lower "gone" entirely. */
    if (hl_vfs_cursor_lookup(&root, "gone", &entry) != -ENOENT) return 3;

    /* `.wh..wh..opq` inside the upper "opq" cuts the lower layer out of the merged directory. */
    if (hl_vfs_cursor_lookup(&root, "opq", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY ||
        !entry.directory.opaque_cut || entry.directory.count != 1) return 4;
    hl_vfs_cursor_entry_release(&entry);

    /* Without an opaque marker the same shape merges both layers. */
    if (hl_vfs_cursor_lookup(&root, "shadow", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY ||
        entry.directory.opaque_cut || entry.directory.count != 2 ||
        entry.directory.layers[0].value.host.handle != 14 ||
        entry.directory.layers[1].value.host.handle != 23) return 5;
    hl_vfs_cursor_entry_release(&entry);

    /* An entry present only in the upper resolves there and merges nothing. */
    if (hl_vfs_cursor_lookup(&root, "kept", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY ||
        entry.directory.count != 1) return 6;
    hl_vfs_cursor_entry_release(&entry);

    if (hl_vfs_cursor_lookup(&root, "absent", &entry) != -ENOENT) return 7;
    hl_vfs_cursor_release(&root);

    /* With the lower layer removed, a whiteout still reports absence -- and the walk must not have
       leaked the probe description it retains on the way. */
    hl_vfs_cursor_authority alone = authority_for(1);
    hl_vfs_cursor single;
    if (hl_vfs_cursor_root_authorities(&alone, NULL, 0, &single) != 0 || single.count != 1) return 8;
    hl_vfs_cursor_authority_close(&alone);
    if (hl_vfs_cursor_lookup(&single, "gone", &entry) != -ENOENT) return 9;
    if (hl_vfs_cursor_lookup(&single, "opq", &entry) != 0 || entry.kind != HL_VFS_CURSOR_DIRECTORY ||
        entry.directory.count != 1) return 10;
    hl_vfs_cursor_entry_release(&entry);
    hl_vfs_cursor_release(&single);

    int handle = leaked();
    if (handle >= 0) return 11;
    return 0;
}
"#,
    )
    .expect("layer probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-Wno-unused-function"])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("layer probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let run = Command::new(&executable).status().expect("layer probe execution");
    assert!(run.success(), "layer probe failed with {run}");
    fs::remove_dir_all(scratch).expect("remove layer probe directory");
}

#[test]
fn fifo_lookup_is_nonblocking_but_read_open_waits_for_a_writer() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-native-cursor-fifo-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("fifo probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(&source, r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include "hl/host_services.h"
#define HL_LINUX_VFS_LOWER_CAPACITY 1
#define HL_NFD 16
static hl_host_result r(int32_t s, uint64_t v) { return (hl_host_result){.status=s,.value=v}; }
static int fd(hl_host_handle h) { return (int)h - 1; }
static hl_host_handle h(int f) { return (hl_host_handle)(f + 1); }
static hl_host_result clone_f(void *c, hl_host_handle x) { (void)c; int y=dup(fd(x)); return y<0?r(HL_STATUS_IO,0):r(HL_STATUS_OK,h(y)); }
static hl_host_result close_f(void *c, hl_host_handle x) { (void)c; return close(fd(x))?r(HL_STATUS_IO,0):r(HL_STATUS_OK,0); }
static hl_host_result open_f(void *c, hl_host_handle d, const char *p, size_t n, uint32_t a, uint32_t cr, uint32_t pm) {
    (void)c;(void)cr;(void)pm; char q[32]; if(n>=sizeof q)return r(HL_STATUS_INVALID_ARGUMENT,0); memcpy(q,p,n);q[n]=0;
    int flags=(a&HL_HOST_FILE_PATH_ONLY)?O_PATH:O_RDONLY;
    if(a&HL_HOST_FILE_NOFOLLOW)flags|=O_NOFOLLOW; if(a&HL_HOST_FILE_DIRECTORY)flags|=O_DIRECTORY;
    int x=openat(fd(d),q,flags|O_CLOEXEC); return x<0?r(errno==ENOENT?HL_STATUS_NOT_FOUND:HL_STATUS_IO,0):r(HL_STATUS_OK,h(x));
}
static hl_host_result meta_f(void *c, hl_host_handle x, hl_host_file_metadata *o) {
    (void)c; struct stat s; if(fstat(fd(x),&s))return r(HL_STATUS_IO,0); memset(o,0,sizeof *o);
    o->stable_device=s.st_dev;o->stable_object=s.st_ino;o->permissions=s.st_mode&07777;
    o->type=S_ISREG(s.st_mode)?HL_HOST_FILE_TYPE_REGULAR:S_ISDIR(s.st_mode)?HL_HOST_FILE_TYPE_DIRECTORY:
            S_ISLNK(s.st_mode)?HL_HOST_FILE_TYPE_SYMLINK:S_ISFIFO(s.st_mode)?HL_HOST_FILE_TYPE_FIFO:HL_HOST_FILE_TYPE_UNKNOWN;
    return r(HL_STATUS_OK,0);
}
static hl_host_result link_f(void *c, hl_host_handle x, hl_host_bytes o) { (void)c; ssize_t n=readlinkat(fd(x),"",o.data,o.size); return n<0?r(HL_STATUS_IO,0):r(HL_STATUS_OK,n); }
static const hl_host_file_services files={.abi=HL_HOST_FILE_ABI,.size=sizeof files,.open_relative=open_f,.metadata=meta_f,.close=close_f,.readlink=link_f,.clone_for_fork=clone_f};
static const hl_host_services services={.abi=HL_HOST_SERVICES_ABI,.size=sizeof services,.file=&files};
#include "linux_abi/container/vfs/cursor.c"
static int hl_vfs_cursor_mount_authority(const char *g, hl_vfs_cursor_authority *o){(void)g;(void)o;return 0;}
static uint64_t ms(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return(uint64_t)t.tv_sec*1000+t.tv_nsec/1000000;}
int main(int ac,char **av){
    if(ac!=2||chdir(av[1])||mkfifo("pipe",0600))return 1;
    int f=open("regular",O_CREAT|O_WRONLY|O_TRUNC,0600);if(f<0||write(f,"regular",7)!=7||close(f)||symlink("regular","link"))return 2;
    int root=open(".",O_PATH|O_DIRECTORY);hl_vfs_cursor_authority a={.kind=HL_VFS_CURSOR_AUTHORITY_HOST,.value.host={.handle=h(root),.services=&services}};hl_vfs_cursor cur;
    if(root<0||hl_vfs_cursor_root_authorities(&a,NULL,0,&cur))return 3;hl_vfs_cursor_authority_close(&a);
    pid_t w=fork();if(w<0)return 4;if(!w){usleep(200000);int z=open("pipe",O_WRONLY);if(z<0||write(z,"Q",1)!=1)_exit(5);close(z);_exit(0);}
    uint64_t t=ms();hl_vfs_cursor_entry e;if(hl_vfs_cursor_lookup(&cur,"pipe",&e)||e.kind!=HL_VFS_CURSOR_FILE)return 6;
    int ws=0;if(ms()-t>=100||waitpid(w,&ws,WNOHANG)!=0)return 7;hl_vfs_cursor_entry_release(&e);
    t=ms();hl_host_result x=open_f(NULL,cur.layers[0].value.host.handle,"pipe",4,HL_HOST_FILE_READ,0,0);char b=0;
    if(x.status!=HL_STATUS_OK||ms()-t<100||read(fd(x.value),&b,1)!=1||b!='Q')return 8;close_f(NULL,x.value);
    if(waitpid(w,&ws,0)!=w||!WIFEXITED(ws)||WEXITSTATUS(ws))return 9;
    if(hl_vfs_cursor_lookup(&cur,"regular",&e)||e.kind!=HL_VFS_CURSOR_FILE)return 10;char text[7];
    if(read(fd(e.file.value.host.handle),text,7)!=7||memcmp(text,"regular",7))return 11;hl_vfs_cursor_entry_release(&e);
    if(hl_vfs_cursor_lookup(&cur,"link",&e)||e.kind!=HL_VFS_CURSOR_SYMLINK||strcmp(e.symlink,"regular"))return 12;
    hl_vfs_cursor_entry_release(&e);hl_vfs_cursor_release(&cur);return 0;
}
"#).expect("fifo probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-unused-function",
            "-Wno-misleading-indentation",
        ])
        .arg(format!("-I{}", native.display()))
        .arg(format!("-I{}", native.join("include").display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("fifo probe compiler");
    assert!(compile.status.success(), "{}", String::from_utf8_lossy(&compile.stderr));
    let mut child = Command::new(&executable)
        .arg(&scratch)
        .spawn()
        .expect("fifo probe execution");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll fifo probe") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill blocked fifo probe");
            let _ = child.wait();
            panic!("fifo probe exceeded five seconds");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "fifo probe failed with {status}");
    fs::remove_dir_all(scratch).expect("remove fifo probe directory");
}
