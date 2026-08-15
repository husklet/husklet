#include "xsave.h"

#include "cpu.h"
#include "guest_data.h"
#include "x87state.h"

#include <string.h>

void hl_x86_xsave_legacy(struct cpu *cpu, uint8_t image[512]) {
    uint64_t saved = cpu->x87_ea;
    if (hl_x87_tags_modelled() && !(cpu->fptop & HL_X87_ARMED)) cpu->fptop |= HL_X87_ARMED | HL_X87_EMPTY_ALL;
    memset(image, 0, 512);
    cpu->x87_ea = (uint64_t)(uintptr_t)image;
    hl_x86_fxsave(cpu);
    cpu->x87_ea = saved;
    uint32_t mxcsr_mask = 0xffffu;
    memcpy(image + 28, &mxcsr_mask, sizeof(mxcsr_mask));
}

uint64_t hl_x86_xsave_xinuse(const uint8_t image[512]) {
    uint64_t inuse = 0;
    uint16_t fcw, fsw;
    uint32_t mxcsr;
    memcpy(&fcw, image, sizeof(fcw));
    memcpy(&fsw, image + 2, sizeof(fsw));
    memcpy(&mxcsr, image + 24, sizeof(mxcsr));
    int x87_init = fcw == 0x037fu && fsw == 0 && image[4] == 0;
    for (unsigned index = 6; index < 24 && x87_init; ++index)
        x87_init = image[index] == 0;
    for (unsigned index = 32; index < 160 && x87_init; ++index)
        x87_init = image[index] == 0;
    int sse_init = mxcsr == 0x1f80u;
    for (unsigned index = 160; index < 416 && sse_init; ++index)
        sse_init = image[index] == 0;
    if (!x87_init) inuse |= 1;
    if (!sse_init) inuse |= 2;
    return inuse;
}

int hl_x86_xsave(struct cpu *cpu, uint64_t *fault_guest) {
    uint64_t guest = cpu->x87_ea;
    if ((guest & 63u) != 0) return -2;

    hl_x86_guest_data_pins pins = {0};
    if (hl_x86_guest_data_prepare_transaction(&pins, guest, HL_X86_XSAVE_SPAN, HL_GUEST_MEMORY_WRITE, fault_guest) != 0)
        return -1;

    uint8_t transaction[HL_X86_XSAVE_SPAN];
    uint8_t legacy[512];
    hl_x86_guest_data_copy_from(&pins, transaction);
    hl_x86_xsave_legacy(cpu, legacy);
    uint64_t rfbm =
        (((cpu->r[RDX] & UINT64_C(0xffffffff)) << 32) | (cpu->r[RAX] & UINT64_C(0xffffffff))) & HL_X86_XSAVE_XCR0;
    if ((rfbm & 1) != 0) {
        memcpy(transaction, legacy, 24);
        memcpy(transaction + 32, legacy + 32, 128);
    }
    if ((rfbm & 2) != 0) {
        memcpy(transaction + 24, legacy + 24, 8);
        memcpy(transaction + 160, legacy + 160, 256);
    }
    uint64_t bv;
    memcpy(&bv, transaction + 512, sizeof(bv));
    bv = (bv & ~rfbm) | (rfbm & hl_x86_xsave_xinuse(legacy));
    memcpy(transaction + 512, &bv, sizeof(bv));
    hl_x86_guest_data_copy_to(&pins, transaction);
    hl_guest_memory_store_observe(guest, HL_X86_XSAVE_SPAN);
    hl_x86_guest_data_release(&pins);
    return 0;
}
