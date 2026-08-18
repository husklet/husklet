use std::{fs, path::PathBuf, process::Command};

#[test]
fn socket_owner_image_rejects_corruption_and_ambiguity() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let native = package.join("src/native");
    let scratch = std::env::temp_dir().join(format!("hl-owner-transport-{}", std::process::id()));
    fs::create_dir_all(&scratch).expect("owner transport probe directory");
    let source = scratch.join("probe.c");
    let executable = scratch.join("probe");
    fs::write(
        &source,
        r#"
#include <errno.h>
#include <stdint.h>
#include <string.h>

#include "linux_abi/container/ownership/transport.c"

static hl_socket_owner_image_header header(hl_socket_owner_image_record *records, size_t count) {
    return (hl_socket_owner_image_header){
        .magic = HL_SOCKET_OWNER_IMAGE_MAGIC,
        .version = HL_SOCKET_OWNER_IMAGE_VERSION,
        .record_size = sizeof *records,
        .count = count,
        .checksum = hl_socket_owner_image_checksum(records, count),
    };
}

int main(void) {
    hl_socket_owner_image_record records[2] = {
        {.object_id = 10, .key = {1, 2, 3}, .uid = 4, .gid = 5, .links = 1, .descriptors = 3},
        {.object_id = 11, .key = {1, 6, 7}, .uid = 8, .gid = 9, .links = 1, .descriptors = 1},
    };
    hl_socket_owner_image_header image = header(records, 2);
    if (hl_socket_owner_image_validate(&image, records, 2) != 0) return 1;
    if (hl_socket_owner_image_validate(&image, records, 1) != EOVERFLOW) return 2;
    records[1].uid++;
    if (hl_socket_owner_image_validate(&image, records, 2) != EBADMSG) return 3;
    records[1].uid--;
    records[1].object_id = records[0].object_id;
    image = header(records, 2);
    if (hl_socket_owner_image_validate(&image, records, 2) != EEXIST) return 4;
    records[1].object_id = 11;
    records[1].key = records[0].key;
    image = header(records, 2);
    if (hl_socket_owner_image_validate(&image, records, 2) != EEXIST) return 5;
    records[1].key = (hl_owner_key){1, 6, 7};
    records[1].descriptors = 0;
    image = header(records, 2);
    if (hl_socket_owner_image_validate(&image, records, 2) != EINVAL) return 6;
    return 0;
}
"#,
    )
    .expect("owner transport probe source");
    let compile = Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(format!("-I{}", native.display()))
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("compile owner transport probe");
    assert!(
        compile.status.success(),
        "owner transport probe did not compile:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&executable).output().expect("run owner transport probe");
    assert!(run.status.success(), "owner transport probe failed with {:?}", run.status.code());
}
