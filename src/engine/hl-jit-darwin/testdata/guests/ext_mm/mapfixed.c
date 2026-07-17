// MAP_FIXED: carve a reservation, then map a fresh anonymous page over its middle at a fixed address.
// The new mapping must land exactly there and be independently writable; neighbours stay intact.
// Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t len = ps * 4;
    char *base = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    memset(base, 0x11, len);

    // replace the 2nd page (offset ps) with a brand-new fixed anon mapping
    char *mid = base + ps;
    char *got = mmap(mid, ps, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_FIXED, -1, 0);
    int placed = got == mid;
    int zeroed = placed && got[0] == 0 && got[ps - 1] == 0;   // fresh anon => zero
    memset(got, 0x22, ps);
    int wrote = (unsigned char)got[0] == 0x22;
    // neighbouring pages untouched by the MAP_FIXED replacement
    int neighbours = (unsigned char)base[0] == 0x11 && (unsigned char)base[ps * 3] == 0x11;
    munmap(base, len);
    printf("mapfixed placed=%d zeroed=%d wrote=%d neighbours=%d\n", placed, zeroed, wrote, neighbours);
    return 0;
}
