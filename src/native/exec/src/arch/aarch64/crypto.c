#include "crypto.h"

#include "stub.h"

#if defined(__linux__) && defined(__aarch64__)
#include <asm/hwcap.h>
#include <sys/auxv.h>
#elif defined(__APPLE__) && defined(__aarch64__)
#include <sys/sysctl.h>
#endif

enum crypto_feature {
    CRYPTO_NONE,
    CRYPTO_AES,
    CRYPTO_SHA1,
    CRYPTO_SHA2,
};

static enum crypto_feature feature(uint32_t word) {
    if ((word & UINT32_C(0xff3e0c00)) == UINT32_C(0x4e280800)) {
        unsigned opcode = (word >> 12) & 31u;
        return ((word >> 22) & 3u) == 0 && opcode >= 4u && opcode <= 7u ? CRYPTO_AES : CRYPTO_NONE;
    }
    if ((word & UINT32_C(0xff3e0c00)) == UINT32_C(0x5e280800)) {
        unsigned opcode = (word >> 12) & 31u;
        if (((word >> 22) & 3u) != 0) return CRYPTO_NONE;
        if (opcode == 0u || opcode == 1u) return CRYPTO_SHA1;
        return opcode == 2u ? CRYPTO_SHA2 : CRYPTO_NONE;
    }
    if ((word & UINT32_C(0xff208c00)) == UINT32_C(0x5e000000)) {
        unsigned opcode = (word >> 12) & 7u;
        if (opcode <= 3u) return CRYPTO_SHA1;
        return opcode <= 6u ? CRYPTO_SHA2 : CRYPTO_NONE;
    }
    return CRYPTO_NONE;
}

#if defined(__APPLE__) && defined(__aarch64__)
static int apple_feature(const char *name) {
    int value = 0;
    size_t size = sizeof(value);
    return sysctlbyname(name, &value, &size, NULL, 0) == 0 && size == sizeof(value) && value != 0;
}
#endif

int hl_a64_crypto_host_supports(uint32_t word) {
    enum crypto_feature required = feature(word);
    if (required == CRYPTO_NONE) return 0;
#if defined(__linux__) && defined(__aarch64__)
    unsigned long capabilities = getauxval(AT_HWCAP);
    if (required == CRYPTO_AES) return (capabilities & HWCAP_AES) != 0;
    if (required == CRYPTO_SHA1) return (capabilities & HWCAP_SHA1) != 0;
    return (capabilities & HWCAP_SHA2) != 0;
#elif defined(__APPLE__) && defined(__aarch64__)
    if (required == CRYPTO_AES) return apple_feature("hw.optional.arm.FEAT_AES");
    if (required == CRYPTO_SHA1) return apple_feature("hw.optional.arm.FEAT_SHA1");
    return apple_feature("hw.optional.arm.FEAT_SHA256");
#else
    (void)word;
    return 0;
#endif
}

int hl_a64_crypto_body(hl_a64_assembler *assembler, uint32_t word) {
    if (assembler == NULL || !hl_a64_crypto_host_supports(word)) return 0;
    hl_a64_emit32(assembler, word);
    return hl_a64_assembler_ok(assembler);
}

int hl_a64_crypto_emit(hl_a64_assembler *assembler, uint32_t word, uint64_t pc) {
    if (assembler == NULL || hl_a64_assembler_remaining(assembler) < HL_A64_CRYPTO_MAX_BYTES ||
        !hl_a64_crypto_host_supports(word))
        return 0;
    hl_a64_stub_prologue(assembler);
    if (!hl_a64_crypto_body(assembler, word)) return 0;
    hl_a64_stub_exit(assembler, HL_NATIVE_EXIT_BRANCH, pc + 4);
    return hl_a64_assembler_ok(assembler);
}
