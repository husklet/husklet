/*
 * Deterministic AArch64 interpreter-census fixture.
 *
 * Build (the runtime corpus performs this exact freestanding link):
 *   aarch64-linux-gnu-gcc -nostdlib -static -no-pie \
 *     -Wl,--build-id=none -Wl,-z,noexecstack -Wl,-e,_start \
 *     terminal.S -o terminal
 *
 * Every instruction is spelled as an encoding so assembler aliases cannot
 * change its top-level decode family.  All conditional branches converge on
 * the immediately following instruction and x0 begins at zero by Linux ABI.
 */

    .section .text
    .balign 4
    .global _start
    .type _start, %function
_start:
    .inst 0x14000001 /* B next                 major 0xa, retired end */
1:
    .inst 0x54000020 /* B.EQ next              major 0xa, retired end */
2:
    .inst 0xb4000020 /* CBZ x0, next           major 0xa, retired end */
3:
    .inst 0x36000020 /* TBZ x0, #0, next       major 0xb, retired end */
4:
    .inst 0x10000041 /* ADR x1, target (+8)    major 0x8, next */
    .inst 0xd61f0020 /* BR x1                  major 0xb, retired end */
target:
    .inst 0xd503201f /* NOP                    major 0xa, next */
    .inst 0xd5033fdf /* ISB                    major 0xa, retired end */
    .inst 0xd50b7520 /* IC IVAU, x0            major 0xa, retired end */
    .inst 0xd2800000 /* MOVZ x0, #0            major 0x9, next */
    .inst 0xd2800ba8 /* MOVZ x8, #93 (__NR_exit), major 0x9, next */
    .inst 0xd4000001 /* SVC #0                 major 0xa, retired end */
    .size _start, .-_start

    .section .note.GNU-stack,"",%progbits
