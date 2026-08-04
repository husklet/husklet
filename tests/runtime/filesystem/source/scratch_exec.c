// #231 scratch/distroless loader guard. A static binary that is the ONLY executable in an otherwise
// EMPTY rootfs (FROM scratch / distroless / nats-server / hello-world's /hello): no shell, no
// interpreter, no libc on disk, nothing but this program. It must exec + run under hl byte-exact with
// the native oracle. Deliberately trivial (fixed output) so the check asserts the LOADER/EXEC path
// resolved argv[0] inside the scratch jail and entered the guest — not any runtime behaviour.
#include <stdio.h>
int main(void) {
    puts("scratch-exec OK");
    return 0;
}
