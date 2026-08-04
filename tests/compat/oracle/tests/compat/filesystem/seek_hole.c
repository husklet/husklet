// lseek SEEK_DATA / SEEK_HOLE on a sparse file: locate the data extent and the hole.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_seekhole_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);

    char block[4096];
    memset(block, 'd', sizeof block);
    // Data in [0,4096); hole in [4096,8192); data in [8192,12288).
    pwrite(fd, block, sizeof block, 0);
    pwrite(fd, block, sizeof block, 8192);
    ftruncate(fd, 12288);

    off_t data_from0 = lseek(fd, 0, SEEK_DATA);
    off_t hole_from0 = lseek(fd, 0, SEEK_HOLE);
    off_t data_after_hole = lseek(fd, 4096, SEEK_DATA);
    off_t hole_after_data = lseek(fd, 8192, SEEK_HOLE);

    // Boolean-ize so tmpfs granularity differences never leak raw geometry.
    int data0 = data_from0 == 0;
    int hole0 = hole_from0 == 4096;
    int data2 = data_after_hole == 8192;
    int hole2 = hole_after_data == 12288;

    close(fd);
    unlink(path);
    rmdir(dir);
    printf("seek-hole data-at-0=%d hole-at-4096=%d data-at-8192=%d hole-at-end=%d\n",
           data0, hole0, data2, hole2);
    return 0;
}
