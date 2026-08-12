#include "operand.h"

uint64_t hl_x86_operand_read(uint64_t address, int width) {
    switch (width) {
    case 1: return *(const uint8_t *)address;
    case 2: return *(const uint16_t *)address;
    case 4: return *(const uint32_t *)address;
    default: return *(const uint64_t *)address;
    }
}
