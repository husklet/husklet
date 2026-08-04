#include <stdio.h>
#include <sys/statvfs.h>
#include <sys/vfs.h>

int main(void) {
    struct statfs fs;
    struct statvfs vfs;
    int statfs_ok = statfs("/tmp", &fs) == 0;
    int statvfs_ok = statvfs("/tmp", &vfs) == 0;
    int geometry = statfs_ok && fs.f_bsize > 0 && fs.f_blocks > 0 && fs.f_bfree <= fs.f_blocks &&
                   fs.f_bavail <= fs.f_blocks && fs.f_namelen > 0;
    int agrees = statfs_ok && statvfs_ok && (unsigned long)fs.f_bsize == vfs.f_bsize &&
                 (unsigned long)fs.f_blocks == vfs.f_blocks;
    printf("statfs ok=%d geometry=%d agrees=%d\n", statfs_ok && statvfs_ok, geometry, agrees);
    return (statfs_ok && statvfs_ok && geometry && agrees) ? 0 : 1;
}
