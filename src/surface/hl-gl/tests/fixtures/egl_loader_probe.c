#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>

typedef void *(*get_display_fn)(void *);
typedef uint32_t (*initialize_fn)(void *, int *, int *);

int main(void) {
    void *library = dlopen("libEGL.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!library) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 2;
    }
    get_display_fn get_display = (get_display_fn)dlsym(library, "eglGetDisplay");
    initialize_fn initialize = (initialize_fn)dlsym(library, "eglInitialize");
    if (!get_display || !initialize) {
        fprintf(stderr, "missing EGL lifecycle symbol\n");
        return 3;
    }
    void *display = get_display(NULL);
    int major = 0, minor = 0;
    uint32_t initialized = initialize(display, &major, &minor);
    printf("display=%p initialized=%u version=%d.%d\n", display, initialized, major, minor);
    return display && initialized && major == 1 && minor == 5 ? 0 : 4;
}
