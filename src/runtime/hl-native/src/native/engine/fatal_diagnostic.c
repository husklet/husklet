#include "fatal_diagnostic.h"

#include <stdatomic.h>
#include <string.h>

static const hl_host_services *hl_fatal_host;
static _Atomic int hl_fatal_armed;

void hl_fatal_diagnostic_init(const hl_host_services *host, const char *selector) {
    int armed = host != NULL && (host->capabilities & HL_HOST_CAP_LOG) != 0 && host->log != NULL &&
                host->log->emit != NULL && selector != NULL && strcmp(selector, "1") == 0;
    hl_fatal_host = armed ? host : NULL;
    atomic_store_explicit(&hl_fatal_armed, armed, memory_order_release);
}

static size_t append_text(char *output, size_t size, const char *text) {
    while (*text != '\0') output[size++] = *text++;
    return size;
}

static size_t append_decimal(char *output, size_t size, uint32_t value) {
    char reverse[10];
    size_t count = 0;
    do {
        reverse[count++] = (char)('0' + value % 10u);
        value /= 10u;
    } while (value != 0);
    while (count != 0) output[size++] = reverse[--count];
    return size;
}

static size_t append_hex(char *output, size_t size, uint64_t value) {
    static const char digits[] = "0123456789abcdef";
    unsigned shift = 60;
    int emitted = 0;
    do {
        unsigned nibble = (unsigned)((value >> shift) & 0xfu);
        if (nibble != 0 || emitted || shift == 0) {
            output[size++] = digits[nibble];
            emitted = 1;
        }
        if (shift == 0) break;
        shift -= 4;
    } while (1);
    return size;
}

#if defined(__GNUC__) || defined(__clang__)
__attribute__((noinline))
#endif
void hl_fatal_diagnostic_publish(uint32_t signal, uint64_t pc, uint64_t sp, uint64_t lr) {
    char output[160];
    size_t size = 0;
    const hl_host_services *host;
    if (!atomic_load_explicit(&hl_fatal_armed, memory_order_acquire)) return;
    host = hl_fatal_host;
    size = append_text(output, size, "fatal-guest-signal signal=");
    size = append_decimal(output, size, signal);
    size = append_text(output, size, " pc=0x");
    size = append_hex(output, size, pc);
    size = append_text(output, size, " sp=0x");
    size = append_hex(output, size, sp);
    size = append_text(output, size, " lr=0x");
    size = append_hex(output, size, lr);
    output[size++] = '\n';
    host->log->emit(host->context, HL_LOG_TAG_SIGNAL, output, size);
}
