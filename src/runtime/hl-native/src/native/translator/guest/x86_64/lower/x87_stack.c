#include "x87_stack.h"

void hl_x87_stack_reset(struct hl_x87_stack *stack) {
    stack->top = 0;
    stack->known = 0;
    stack->dirty = 0;
}

void hl_x87_stack_anchor(struct hl_x87_stack *stack, unsigned top) {
    stack->top = top & 7u;
    stack->known = 1;
    stack->dirty = 0;
}

int hl_x87_stack_known(const struct hl_x87_stack *stack) {
    return stack->known != 0;
}

int hl_x87_stack_dirty(const struct hl_x87_stack *stack) {
    return stack->dirty != 0;
}

unsigned hl_x87_stack_top(const struct hl_x87_stack *stack) {
    return stack->top;
}

unsigned hl_x87_stack_slot(const struct hl_x87_stack *stack, int index) {
    return (stack->top + (unsigned)index) & 7u;
}

void hl_x87_stack_push(struct hl_x87_stack *stack) {
    if (!stack->known) return;
    stack->top = (stack->top - 1u) & 7u;
    stack->dirty = 1;
}

void hl_x87_stack_adjust(struct hl_x87_stack *stack, int delta) {
    if (!stack->known) return;
    stack->top = (stack->top + (unsigned)delta) & 7u;
    stack->dirty = 1;
}

void hl_x87_stack_materialized(struct hl_x87_stack *stack) {
    stack->dirty = 0;
}

void hl_x87_stack_drop(struct hl_x87_stack *stack) {
    stack->known = 0;
    stack->dirty = 0;
}
