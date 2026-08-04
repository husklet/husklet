#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <arm_neon.h>

__attribute__((target("+i8mm"))) static void go(int32_t out[16]) {
    int8_t sa[16], sb[16];
    uint8_t ua[16], ub[16];
    for (int i = 0; i < 16; i++) {
        sa[i] = (int8_t)((i * 17 + 3) % 23 - 11);
        sb[i] = (int8_t)((i * 11 + 5) % 19 - 9);
        ua[i] = (uint8_t)(129 + i * 7);
        ub[i] = (uint8_t)(251 - i * 9);
    }
    vst1q_s32(out, vmmlaq_s32(vdupq_n_s32(7), vld1q_s8(sa), vld1q_s8(sb)));
    vst1q_u32((uint32_t *)(out + 4), vmmlaq_u32(vdupq_n_u32(11), vld1q_u8(ua), vld1q_u8(ub)));
    vst1q_s32(out + 8, vusmmlaq_s32(vdupq_n_s32(-13), vld1q_u8(ua), vld1q_s8(sb)));
    int8x16_t alias = vld1q_s8(sa), b = vld1q_s8(sb);
    __asm__("smmla %0.4s,%0.16b,%1.16b" : "+w"(alias) : "w"(b));
    vst1q_s32(out + 12, vreinterpretq_s32_s8(alias));
}

int main(void) {
    int32_t out[16];
    go(out);
    printf("i8mm");
    for (int i = 0; i < 16; i++)
        printf(" %d", out[i]);
    putchar('\n');
    return 0;
}
