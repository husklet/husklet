#if defined(__aarch64__)
/* Mechanically retained from engine/src/core/dispatch.c.  x18 is reserved by
 * platform ABIs and is intentionally neither read nor written. */
.text
.p2align 2
.hidden hl_native_aarch64_enter
.global hl_native_aarch64_enter
.type hl_native_aarch64_enter,%function
hl_native_aarch64_enter:
    stp x19,x20,[x0,#288]
    stp x21,x22,[x0,#304]
    stp x23,x24,[x0,#320]
    stp x25,x26,[x0,#336]
    stp x27,x28,[x0,#352]
    stp x29,x30,[x0,#368]
    stp q8,q9,[x0,#896]
    stp q10,q11,[x0,#928]
    stp q12,q13,[x0,#960]
    stp q14,q15,[x0,#992]
    sub sp,sp,#16
    mrs x9,fpcr
    mrs x10,fpsr
    stp x9,x10,[sp]
    /* host_stack is the caller's sp; the fpcr/fpsr pair sits just below it. */
    add x9,sp,#16
    str x9,[x0,#280]
    /* Prove the view table once per entry -- published, current, in capacity --
     * and leave the usable count at 2400, zero when it cannot be trusted. */
    add x9,x0,#1688
    ldar x9,[x9]
    ldr x10,[x0,#1696]
    cmp x9,#0
    ccmp x9,x10,#0,ne
    ldr x9,[x0,#1704]
    csel x9,x9,xzr,eq
    cmp x9,#4
    csel x9,x9,xzr,ls
    str x9,[x0,#2400]
    br x1
.size hl_native_aarch64_enter,.-hl_native_aarch64_enter

.p2align 2
.hidden hl_native_aarch64_return
.global hl_native_aarch64_return
.type hl_native_aarch64_return,%function
hl_native_aarch64_return:
    ldp x19,x20,[x0,#288]
    ldp x21,x22,[x0,#304]
    ldp x23,x24,[x0,#320]
    ldp x25,x26,[x0,#336]
    ldp x27,x28,[x0,#352]
    ldp x29,x30,[x0,#368]
    ldp q8,q9,[x0,#896]
    ldp q10,q11,[x0,#928]
    ldp q12,q13,[x0,#960]
    ldp q14,q15,[x0,#992]
    ldr x9,[x0,#280]
    ldp x10,x11,[x9,#-16]
    mov sp,x9
    msr fpcr,x10
    msr fpsr,x11
    ret
.size hl_native_aarch64_return,.-hl_native_aarch64_return

/* Signal return resumes here with x0=cpu. Unlike a generated exit this must
 * not spill live host registers: fault capture already reconstructed exact
 * guest pre-op state in the CPU record. */
.p2align 2
.hidden hl_native_aarch64_fault_return
.global hl_native_aarch64_fault_return
.type hl_native_aarch64_fault_return,%function
hl_native_aarch64_fault_return:
    b hl_native_aarch64_return
.size hl_native_aarch64_fault_return,.-hl_native_aarch64_fault_return

/* Fully spilled, semantics-free block used while translation is unavailable.
 * The generated layout fixes program at 256.  Loading and storing the same
 * value proves the CPU-record seam without advancing architectural state. */
.p2align 2
.hidden hl_native_aarch64_fallback
.global hl_native_aarch64_fallback
.type hl_native_aarch64_fallback,%function
hl_native_aarch64_fallback:
    ldr x9,[x0,#256]
    str x9,[x0,#256]
    b hl_native_aarch64_return
.size hl_native_aarch64_fallback,.-hl_native_aarch64_fallback
#endif
