#ifndef HL_TRANSLATOR_X86_64_X87_STACK_H
#define HL_TRANSLATOR_X86_64_X87_STACK_H

struct hl_x87_stack {
    unsigned top;
    unsigned known;
    unsigned dirty;
};

void hl_x87_stack_reset(struct hl_x87_stack *stack);
void hl_x87_stack_anchor(struct hl_x87_stack *stack, unsigned top);
int hl_x87_stack_known(const struct hl_x87_stack *stack);
int hl_x87_stack_dirty(const struct hl_x87_stack *stack);
unsigned hl_x87_stack_top(const struct hl_x87_stack *stack);
unsigned hl_x87_stack_slot(const struct hl_x87_stack *stack, int index);
void hl_x87_stack_push(struct hl_x87_stack *stack);
void hl_x87_stack_adjust(struct hl_x87_stack *stack, int delta);
void hl_x87_stack_materialized(struct hl_x87_stack *stack);
void hl_x87_stack_drop(struct hl_x87_stack *stack);

#endif
