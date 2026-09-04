.text
.global _start
.type _start,%function
_start:
    movz x0,#0
1:
    add x0,x0,#1
    cbnz x0,1b
    movz x0,#93
    movz x8,#93
    svc #0
.size _start,.-_start
