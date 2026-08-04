#if defined(__aarch64__)
.text
.p2align 2
.global hl_x86_test_enter
.type hl_x86_test_enter,%function
hl_x86_test_enter:
    sub sp,sp,#96
    stp x19,x20,[sp,#0]
    stp x21,x22,[sp,#16]
    stp x23,x24,[sp,#32]
    stp x25,x26,[sp,#48]
    stp x27,x28,[sp,#64]
    stp x29,x30,[sp,#80]
    mov x28,x0
    mov x27,x1
    ldp x0,x1,[x28,#0]
    ldp x2,x3,[x28,#16]
    ldp x4,x5,[x28,#32]
    ldp x6,x7,[x28,#48]
    ldp x8,x9,[x28,#64]
    ldp x10,x11,[x28,#80]
    ldp x12,x13,[x28,#96]
    ldp x14,x15,[x28,#112]
    blr x27
    ldp x19,x20,[sp,#0]
    ldp x21,x22,[sp,#16]
    ldp x23,x24,[sp,#32]
    ldp x25,x26,[sp,#48]
    ldp x27,x28,[sp,#64]
    ldp x29,x30,[sp,#80]
    add sp,sp,#96
    ret
.size hl_x86_test_enter,.-hl_x86_test_enter

.p2align 2
.global hl_x86_test_preserved
.type hl_x86_test_preserved,%function
hl_x86_test_preserved:
    sub sp,sp,#160
    stp x19,x20,[sp,#0]
    stp x29,x30,[sp,#16]
    stp q8,q9,[sp,#32]
    stp q10,q11,[sp,#64]
    stp q12,q13,[sp,#96]
    stp q14,q15,[sp,#128]
    mov x19,x3
    ldp q8,q9,[x2,#0]
    ldp q10,q11,[x2,#32]
    ldp q12,q13,[x2,#64]
    ldp q14,q15,[x2,#96]
    bl hl_native_x86_64_enter
    stp q8,q9,[x19,#0]
    stp q10,q11,[x19,#32]
    stp q12,q13,[x19,#64]
    stp q14,q15,[x19,#96]
    ldp q8,q9,[sp,#32]
    ldp q10,q11,[sp,#64]
    ldp q12,q13,[sp,#96]
    ldp q14,q15,[sp,#128]
    ldp x19,x20,[sp,#0]
    ldp x29,x30,[sp,#16]
    add sp,sp,#160
    ret
.size hl_x86_test_preserved,.-hl_x86_test_preserved
#endif
