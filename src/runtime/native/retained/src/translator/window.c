#include "window.h"

int hl_window_contains(uint64_t extent, uint64_t offset, uint64_t width, uint64_t alignment) {
    if (alignment == 0 || offset % alignment != 0) return 0;
    return offset <= extent && width <= extent - offset;
}
