#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_X87_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_X87_H

#include <stdint.h>

void hl_x86_x87_reset(void);
void hl_x86_x87_anchor(unsigned top);
int hl_x86_x87_optimized(void);
int hl_x86_x87_known(void);
void hl_x86_x87_materialize(void);
void hl_x86_x87_drop(void);
void hl_x86_x87_load(int destination, int index);
void hl_x86_x87_store(int source, int index);
void hl_x86_x87_push(int source);
void hl_x86_x87_pop(void);
void hl_x86_x87_adjust_top(int delta);
void hl_x86_x87_live(int a, int b);
void hl_x86_x87_indefinite(int destination);
void hl_x86_x87_tag(int index, int empty);
void hl_x86_x87_rc_enter(void);
void hl_x86_x87_rc_leave(void);
void hl_x86_x87_narrow(int destination);
void hl_x86_x87_rounded_up(int rounded, int original);
void hl_x86_x87_denormal(int bits, int single);
void hl_x86_x87_dnan_pre(int n, int m);
void hl_x86_x87_dnan_post(int destination);
void hl_x86_x87_scale(void);
void hl_x86_x87_extract(void);
void hl_x86_x87_round(void);
void hl_x86_x87_test(void);
void hl_x86_x87_status(void);
void hl_x86_x87_clear_exceptions(void);
void hl_x86_x87_classify(void);
void hl_x86_x87_function(int function, uint64_t next);

#endif
