#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/sendfile.h>
#include <unistd.h>

int main(void) {
    char input_path[] = "/tmp/hl-sendfile-in-XXXXXX";
    char output_path[] = "/tmp/hl-sendfile-out-XXXXXX";
    int input = mkstemp(input_path);
    int output = mkstemp(output_path);
    if (input < 0 || output < 0 || write(input, "abcdefgh", 8) != 8 || lseek(input, 2, SEEK_SET) != 2) return 1;
    int64_t current_count = sendfile(output, input, NULL, 3);
    int current_ok = current_count == 3 && lseek(input, 0, SEEK_CUR) == 5;
    off_t explicit_offset = 5;
    int64_t partial_count = sendfile(output, input, &explicit_offset, 100);
    int partial_ok = partial_count == 3 && explicit_offset == 8 && lseek(input, 0, SEEK_CUR) == 5;
    errno = 0;
    int efault_ok = sendfile(output, input, (off_t *)(uintptr_t)1, 1) == -1 && errno == EFAULT;
    char result[7] = {0};
    int bytes_ok = pread(output, result, 6, 0) == 6 && memcmp(result, "cdefgh", 6) == 0;
    printf("sendfile-edge current=%d partial=%d efault=%d bytes=%d\n", current_ok, partial_ok, efault_ok, bytes_ok);
    close(input);
    close(output);
    unlink(input_path);
    unlink(output_path);
    return current_ok && partial_ok && efault_ok && bytes_ok ? 0 : 2;
}
