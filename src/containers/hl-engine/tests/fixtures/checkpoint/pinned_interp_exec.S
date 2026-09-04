.global _start
.section .text
_start:
    mov %rsp, %r13
    mov (%rsp), %r12
    cmp $1, %r12
    je first
    cmp $2, %r12
    je before
after:
    lea after_msg(%rip), %rsi
    mov $1, %edi
    mov $after_len, %edx
    mov $1, %eax
    syscall
    xor %edi, %edi
    mov $60, %eax
    syscall
first:
    mov 8(%rsp), %rdi
    lea before_arg(%rip), %rax
    push $0
    push %rax
    push %rdi
    mov %rsp, %rsi
    lea 16(%r13,%r12,8), %rdx
    mov $59, %eax
    syscall
    jmp fail
before:
    lea before_msg(%rip), %rsi
    mov $1, %edi
    mov $before_len, %edx
    mov $1, %eax
    syscall
    sub $8, %rsp
    xor %edi, %edi
    mov %rsp, %rsi
    mov $1, %edx
    xor %eax, %eax
    syscall
    add $8, %rsp
    mov 8(%rsp), %rdi
    lea before_arg(%rip), %rax
    lea after_arg(%rip), %rcx
    push $0
    push %rcx
    push %rax
    push %rdi
    mov %rsp, %rsi
    lea 16(%r13,%r12,8), %rdx
    mov $59, %eax
    syscall
fail:
    mov $111, %edi
    mov $60, %eax
    syscall
.section .rodata
before_arg: .asciz "before"
after_arg: .asciz "after"
before_msg: .ascii "PINNED-INTERP-BEFORE-CAPTURE:5d0d8e2f\n"
.set before_len, .-before_msg
after_msg: .ascii "PINNED-INTERP-AFTER-RESTORE:5d0d8e2f\n"
.set after_len, .-after_msg
