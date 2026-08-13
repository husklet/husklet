#include "avx_internal.h"
#include "rep_runtime.h"

#include <limits.h>
#include <string.h>

static enum avx_dispatch_result avx_dispatch_map1_packed_integer_arithmetic(const hl_x86_avx_state *state,
                                                                            struct cpu *c, struct insn *instruction,
                                                                            uint64_t next, int width) {
    int op = instruction->op;
    int source = instruction->vvvv;
    uint8_t left[64], right[64], result[64];
    int supported = op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4 || (op >= 0xF8 && op <= 0xFB) || op == 0xEC ||
                    op == 0xED || op == 0xE8 || op == 0xE9 || op == 0xDC || op == 0xDD || op == 0xD8 || op == 0xD9 ||
                    op == 0xDA || op == 0xDE || op == 0xEA || op == 0xEE || op == 0xE0 || op == 0xE3 || op == 0xD5 ||
                    op == 0xE5 || op == 0xE4 || op == 0xF5 || op == 0xF6;
    if (!supported) return AVX_DISPATCH_UNMATCHED;
    avx_get(c, source, left);
    avx_get_rm(state, c, instruction, next, width, right);

    if (op == 0xFC || op == 0xFD || op == 0xFE || op == 0xD4 ||
        (op >= 0xF8 && op <= 0xFB)) { // vpaddb/w/d/q and vpsubb/w/d/q
        int element = (op == 0xFC || op == 0xF8)   ? 1
                      : (op == 0xFD || op == 0xF9) ? 2
                      : (op == 0xFE || op == 0xFA) ? 4
                                                   : 8;
        int subtract = op >= 0xF8 && op <= 0xFB;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            uint64_t value = subtract ? x - y : x + y;
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xEC || op == 0xED || op == 0xE8 || op == 0xE9 || op == 0xDC || op == 0xDD || op == 0xD8 ||
               op == 0xD9) { // signed/unsigned saturating add/sub
        int word = op == 0xED || op == 0xE9 || op == 0xDD || op == 0xD9;
        int uns = op == 0xDC || op == 0xDD || op == 0xD8 || op == 0xD9;
        int subtract = op == 0xE8 || op == 0xE9 || op == 0xD8 || op == 0xD9;
        int element = word ? 2 : 1;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            int64_t value;
            if (uns) {
                int64_t candidate = subtract ? (int64_t)x - (int64_t)y : (int64_t)x + (int64_t)y;
                int64_t maximum = word ? 65535 : 255;
                value = candidate < 0 ? 0 : candidate > maximum ? maximum : candidate;
            } else {
                int shift = 64 - element * 8;
                int64_t signed_x = ((int64_t)x << shift) >> shift;
                int64_t signed_y = ((int64_t)y << shift) >> shift;
                int64_t candidate = subtract ? signed_x - signed_y : signed_x + signed_y;
                int64_t minimum = word ? -32768 : -128;
                int64_t maximum = word ? 32767 : 127;
                value = candidate < minimum ? minimum : candidate > maximum ? maximum : candidate;
            }
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xDA || op == 0xDE || op == 0xEA || op == 0xEE) { // pminub/pmaxub/pminsw/pmaxsw
        int word = op == 0xEA || op == 0xEE;
        int maximum = op == 0xDE || op == 0xEE;
        if (word) {
            for (int offset = 0; offset < width; offset += 2) {
                int16_t x, y;
                memcpy(&x, left + offset, 2);
                memcpy(&y, right + offset, 2);
                int16_t value = maximum ? (x > y ? x : y) : (x < y ? x : y);
                memcpy(result + offset, &value, 2);
            }
        } else {
            for (int offset = 0; offset < width; offset++) {
                uint8_t x = left[offset], y = right[offset];
                result[offset] = maximum ? (x > y ? x : y) : (x < y ? x : y);
            }
        }
    } else if (op == 0xE0 || op == 0xE3) { // pavgb/pavgw
        int element = op == 0xE0 ? 1 : 2;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t x = 0, y = 0;
            memcpy(&x, left + offset, (size_t)element);
            memcpy(&y, right + offset, (size_t)element);
            uint64_t value = (x + y + 1) >> 1;
            memcpy(result + offset, &value, (size_t)element);
        }
    } else if (op == 0xD5 || op == 0xE5 || op == 0xE4) { // pmullw/pmulhw/pmulhuw
        for (int offset = 0; offset < width; offset += 2) {
            uint16_t x, y;
            memcpy(&x, left + offset, 2);
            memcpy(&y, right + offset, 2);
            uint16_t value = op == 0xD5   ? (uint16_t)(x * y)
                             : op == 0xE4 ? (uint16_t)(((uint32_t)x * (uint32_t)y) >> 16)
                                          : (uint16_t)(((int32_t)(int16_t)x * (int32_t)(int16_t)y) >> 16);
            memcpy(result + offset, &value, 2);
        }
    } else if (op == 0xF5) { // vpmaddwd
        for (int offset = 0; offset < width; offset += 4) {
            int16_t x0, x1, y0, y1;
            memcpy(&x0, left + offset, 2);
            memcpy(&x1, left + offset + 2, 2);
            memcpy(&y0, right + offset, 2);
            memcpy(&y1, right + offset + 2, 2);
            int32_t value = (int32_t)x0 * (int32_t)y0 + (int32_t)x1 * (int32_t)y1;
            memcpy(result + offset, &value, 4);
        }
    } else if (op == 0xF6) { // vpsadbw
        memset(result, 0, sizeof(result));
        for (int block = 0; block < width; block += 8) {
            int sum = 0;
            for (int offset = 0; offset < 8; offset++) {
                int difference = (int)left[block + offset] - (int)right[block + offset];
                sum += difference < 0 ? -difference : difference;
            }
            uint16_t value = (uint16_t)sum;
            memcpy(result + block, &value, 2);
        }
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_horizontal_floating(const hl_x86_avx_state *state, struct cpu *c,
                                                                      struct insn *instruction, uint64_t next, int map,
                                                                      int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0x7C && op != 0x7D && op != 0xD0)) return AVX_DISPATCH_UNMATCHED;
    int dbl = instruction->vex_pp == 1;
    int subtract = op == 0x7D;
    uint8_t left[64], right[64], result[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    for (int lane = 0; lane < width; lane += 16) {
        if (!dbl) {
            float x[4], y[4], output[4];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            if (op == 0xD0) {
                for (int element = 0; element < 4; element++)
                    output[element] = (element & 1) ? avx_dnan_f32(x[element] + y[element], x[element], y[element])
                                                    : avx_dnan_f32(x[element] - y[element], x[element], y[element]);
            } else {
                output[0] = subtract ? avx_dnan_f32(x[0] - x[1], x[0], x[1]) : avx_dnan_f32(x[0] + x[1], x[0], x[1]);
                output[1] = subtract ? avx_dnan_f32(x[2] - x[3], x[2], x[3]) : avx_dnan_f32(x[2] + x[3], x[2], x[3]);
                output[2] = subtract ? avx_dnan_f32(y[0] - y[1], y[0], y[1]) : avx_dnan_f32(y[0] + y[1], y[0], y[1]);
                output[3] = subtract ? avx_dnan_f32(y[2] - y[3], y[2], y[3]) : avx_dnan_f32(y[2] + y[3], y[2], y[3]);
            }
            memcpy(result + lane, output, 16);
        } else {
            double x[2], y[2], output[2];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            if (op == 0xD0) {
                output[0] = avx_dnan_f64(x[0] - y[0], x[0], y[0]);
                output[1] = avx_dnan_f64(x[1] + y[1], x[1], y[1]);
            } else {
                output[0] = subtract ? avx_dnan_f64(x[0] - x[1], x[0], x[1]) : avx_dnan_f64(x[0] + x[1], x[0], x[1]);
                output[1] = subtract ? avx_dnan_f64(y[0] - y[1], y[0], y[1]) : avx_dnan_f64(y[0] + y[1], y[0], y[1]);
            }
            memcpy(result + lane, output, 16);
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_scalar_shift(const hl_x86_avx_state *state, struct cpu *c,
                                                               struct insn *instruction, uint64_t next, int map,
                                                               int width) {
    int op = instruction->op;
    int supported =
        op == 0xD1 || op == 0xD2 || op == 0xD3 || op == 0xE1 || op == 0xE2 || op == 0xF1 || op == 0xF2 || op == 0xF3;
    if (map != 1 || !supported) return AVX_DISPATCH_UNMATCHED;
    int element = (op == 0xD1 || op == 0xE1 || op == 0xF1) ? 2 : (op == 0xD2 || op == 0xE2 || op == 0xF2) ? 4 : 8;
    int arithmetic = op == 0xE1 || op == 0xE2;
    int left = op == 0xF1 || op == 0xF2 || op == 0xF3;
    uint8_t source[64], count_source[64], result[64];
    avx_get(c, instruction->vvvv, source);
    avx_get_rm(state, c, instruction, next, 16, count_source);
    uint64_t count;
    memcpy(&count, count_source, 8);
    int bits = element * 8;
    for (int offset = 0; offset < width; offset += element) {
        uint64_t value = 0;
        memcpy(&value, source + offset, (size_t)element);
        uint64_t shifted;
        if (count >= (uint64_t)bits) {
            if (arithmetic) {
                int64_t sign_shift = 64 - bits;
                shifted = (uint64_t)(((int64_t)(value << sign_shift) >> sign_shift) < 0 ? ~0ull : 0ull);
            } else {
                shifted = 0;
            }
        } else if (left) {
            shifted = value << count;
        } else if (arithmetic) {
            int64_t sign_shift = 64 - bits;
            int64_t signed_value = ((int64_t)(value << sign_shift)) >> sign_shift;
            shifted = (uint64_t)(signed_value >> count);
        } else {
            shifted = value >> count;
        }
        memcpy(result + offset, &shifted, (size_t)element);
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_immediate_shift(struct cpu *c, struct insn *instruction,
                                                                  uint64_t next, int map, int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0x71 && op != 0x72 && op != 0x73)) return AVX_DISPATCH_UNMATCHED;
    int extension = instruction->reg;
    int immediate = (uint8_t)instruction->imm;
    int element = op == 0x71 ? 2 : op == 0x72 ? 4 : 8;
    uint8_t source[64], result[64];
    avx_get(c, instruction->rm_reg, source);
    if (op == 0x73 && (extension == 3 || extension == 7)) {
        for (int lane = 0; lane < width; lane += 16)
            for (int offset = 0; offset < 16; offset++) {
                if (extension == 3)
                    result[lane + offset] =
                        immediate < 16 && offset < 16 - immediate ? source[lane + offset + immediate] : 0;
                else
                    result[lane + offset] =
                        immediate < 16 && offset >= immediate ? source[lane + offset - immediate] : 0;
            }
    } else {
        int left = extension == 6;
        int arithmetic = extension == 4;
        int bits = element * 8;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t value = 0, shifted;
            memcpy(&value, source + offset, (size_t)element);
            if (left) {
                shifted = immediate >= bits ? 0 : value << immediate;
            } else if (arithmetic) {
                int sign_shift = 64 - bits;
                int64_t signed_value = ((int64_t)value << sign_shift) >> sign_shift;
                shifted = (uint64_t)(signed_value >> (immediate >= bits ? bits - 1 : immediate));
            } else {
                shifted = immediate >= bits ? 0 : value >> immediate;
            }
            memcpy(result + offset, &shifted, (size_t)element);
        }
    }
    avx_put(c, instruction->vvvv, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_packed_numeric_conversion(const hl_x86_avx_state *state, struct cpu *c,
                                                                       struct insn *instruction, uint64_t next, int map,
                                                                       int op, int destination, int width) {
    if (map != 1 || (op != 0x5B && op != 0xE6)) return AVX_DISPATCH_UNMATCHED;
    int prefix = instruction->vex_pp;
    uint8_t source[64], output[64];
    if (op == 0x5B) {
        avx_get_rm(state, c, instruction, next, width, source);
        for (int offset = 0; offset < width; offset += 4) {
            if (prefix == 0) {
                int32_t value;
                memcpy(&value, source + offset, 4);
                float converted = (float)value;
                memcpy(output + offset, &converted, 4);
            } else {
                float value;
                memcpy(&value, source + offset, 4);
                int32_t converted = (int32_t)cvt_x86_f2i(value, prefix == 2, 0);
                memcpy(output + offset, &converted, 4);
            }
        }
        avx_put(c, destination, output, width);
    } else if (prefix == 2) {
        avx_get_rm(state, c, instruction, next, width / 2, source);
        for (int index = 0; index < width / 8; index++) {
            int32_t value;
            memcpy(&value, source + 4 * index, 4);
            double converted = (double)value;
            memcpy(output + 8 * index, &converted, 8);
        }
        avx_put(c, destination, output, width);
    } else {
        avx_get_rm(state, c, instruction, next, width, source);
        for (int index = 0; index < width / 8; index++) {
            double value;
            memcpy(&value, source + 8 * index, 8);
            int32_t converted = (int32_t)cvt_x86_d2i(value, prefix == 1, 0);
            memcpy(output + 4 * index, &converted, 4);
        }
        avx_put(c, destination, output, width / 2);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map1_immediate_shuffle(const hl_x86_avx_state *state, struct cpu *c,
                                                                    struct insn *instruction, uint64_t next, int map,
                                                                    int width) {
    int op = instruction->op;
    if (map != 1 || (op != 0xC6 && op != 0x70)) return AVX_DISPATCH_UNMATCHED;
    int immediate = (uint8_t)instruction->imm;
    int prefix = instruction->vex_pp;
    uint8_t left[64], right[64], result[64];
    if (op == 0xC6) {
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        if (prefix == 1) {
            for (int qword = 0; qword < width / 8; qword++) {
                const uint8_t *source = (qword & 1) ? right : left;
                int lane = (qword / 2) * 16;
                memcpy(result + qword * 8, source + lane + (((immediate >> qword) & 1) ? 8 : 0), 8);
            }
        } else {
            for (int lane = 0; lane < width; lane += 16) {
                memcpy(result + lane, left + lane + 4 * (immediate & 3), 4);
                memcpy(result + lane + 4, left + lane + 4 * ((immediate >> 2) & 3), 4);
                memcpy(result + lane + 8, right + lane + 4 * ((immediate >> 4) & 3), 4);
                memcpy(result + lane + 12, right + lane + 4 * ((immediate >> 6) & 3), 4);
            }
        }
    } else {
        avx_get_rm(state, c, instruction, next, width, right);
        for (int lane = 0; lane < width; lane += 16) {
            if (prefix == 1) {
                for (int dword = 0; dword < 4; dword++) {
                    int selected = (immediate >> (2 * dword)) & 3;
                    memcpy(result + lane + 4 * dword, right + lane + 4 * selected, 4);
                }
            } else {
                memcpy(result + lane, right + lane, 16);
                int base = prefix == 3 ? 8 : 0;
                for (int word = 0; word < 4; word++) {
                    int selected = (immediate >> (2 * word)) & 3;
                    memcpy(result + lane + base + 2 * word, right + lane + base + 2 * selected, 2);
                }
            }
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_horizontal_integer(const hl_x86_avx_state *state, struct cpu *c,
                                                                     struct insn *instruction, uint64_t next, int map,
                                                                     int width) {
    int op = instruction->op;
    int supported = op == 0x01 || op == 0x02 || op == 0x03 || op == 0x05 || op == 0x06 || op == 0x07;
    if (map != 2 || !supported) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], result[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    int subtract = op >= 0x05;
    int saturate = op == 0x03 || op == 0x07;
    int dword = op == 0x02 || op == 0x06;
    for (int lane = 0; lane < width; lane += 16) {
        if (dword) {
            int32_t x[4], y[4], output[4];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            output[0] = subtract ? x[0] - x[1] : x[0] + x[1];
            output[1] = subtract ? x[2] - x[3] : x[2] + x[3];
            output[2] = subtract ? y[0] - y[1] : y[0] + y[1];
            output[3] = subtract ? y[2] - y[3] : y[2] + y[3];
            memcpy(result + lane, output, 16);
        } else {
            int16_t x[8], y[8], output[8];
            memcpy(x, left + lane, 16);
            memcpy(y, right + lane, 16);
            for (int pair = 0; pair < 4; pair++) {
                int left_value = subtract ? x[2 * pair] - x[2 * pair + 1] : x[2 * pair] + x[2 * pair + 1];
                int right_value = subtract ? y[2 * pair] - y[2 * pair + 1] : y[2 * pair] + y[2 * pair + 1];
                output[pair] = saturate ? (int16_t)sat_s16(left_value) : (int16_t)left_value;
                output[pair + 4] = saturate ? (int16_t)sat_s16(right_value) : (int16_t)right_value;
            }
            memcpy(result + lane, output, 16);
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_minimum_maximum(const hl_x86_avx_state *state, struct cpu *c,
                                                                  struct insn *instruction, uint64_t next, int map,
                                                                  int op, int width) {
    if (map != 2 || op < 0x38 || op > 0x3F) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], output[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    int maximum = op >= 0x3C;
    if (op == 0x38 || op == 0x3C) {
        for (int offset = 0; offset < width; offset++) {
            int8_t x = (int8_t)left[offset], y = (int8_t)right[offset];
            output[offset] = (uint8_t)(maximum ? (x > y ? x : y) : (x < y ? x : y));
        }
    } else if (op == 0x3A || op == 0x3E) {
        for (int offset = 0; offset < width; offset += 2) {
            uint16_t x, y;
            memcpy(&x, left + offset, 2);
            memcpy(&y, right + offset, 2);
            uint16_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 2);
        }
    } else if (op == 0x39 || op == 0x3D) {
        for (int offset = 0; offset < width; offset += 4) {
            int32_t x, y;
            memcpy(&x, left + offset, 4);
            memcpy(&y, right + offset, 4);
            int32_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 4);
        }
    } else {
        for (int offset = 0; offset < width; offset += 4) {
            uint32_t x, y;
            memcpy(&x, left + offset, 4);
            memcpy(&y, right + offset, 4);
            uint32_t result = maximum ? (x > y ? x : y) : (x < y ? x : y);
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, instruction->reg, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_minimum_position(const hl_x86_avx_state *state, struct cpu *c,
                                                                   struct insn *instruction, uint64_t next, int map,
                                                                   int op) {
    if (map != 2 || op != 0x41) return AVX_DISPATCH_UNMATCHED;
    uint16_t words[32];
    avx_get_rm(state, c, instruction, next, 16, (uint8_t *)words);
    uint16_t best = words[0];
    int index = 0;
    for (int candidate = 1; candidate < 8; candidate++)
        if (words[candidate] < best) {
            best = words[candidate];
            index = candidate;
        }
    uint16_t output[32] = {best, (uint16_t)index};
    avx_put(c, instruction->reg, (uint8_t *)output, 16);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_ssse3_arithmetic(const hl_x86_avx_state *state, struct cpu *c,
                                                                   struct insn *instruction, uint64_t next, int map,
                                                                   int width) {
    int op = instruction->op;
    int supported = op == 0x04 || (op >= 0x08 && op <= 0x0B) || (op >= 0x1C && op <= 0x1E);
    if (map != 2 || !supported) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], result[64];
    if (op >= 0x1C) {
        avx_get_rm(state, c, instruction, next, width, right);
        int element = op == 0x1C ? 1 : op == 0x1D ? 2 : 4;
        for (int offset = 0; offset < width; offset += element) {
            uint64_t value = 0;
            memcpy(&value, right + offset, (size_t)element);
            uint64_t output = simd_element_negative(value, element) ? simd_element_negate(value, element) : value;
            memcpy(result + offset, &output, (size_t)element);
        }
    } else {
        avx_get(c, instruction->vvvv, left);
        avx_get_rm(state, c, instruction, next, width, right);
        if (op == 0x04) {
            for (int lane = 0; lane < width; lane += 16) {
                int16_t output[8];
                for (int pair = 0; pair < 8; pair++) {
                    int product = (int)(uint8_t)left[lane + 2 * pair] * (int)(int8_t)right[lane + 2 * pair] +
                                  (int)(uint8_t)left[lane + 2 * pair + 1] * (int)(int8_t)right[lane + 2 * pair + 1];
                    output[pair] = (int16_t)sat_s16(product);
                }
                memcpy(result + lane, output, 16);
            }
        } else if (op <= 0x0A) {
            int element = op == 0x08 ? 1 : op == 0x09 ? 2 : 4;
            for (int offset = 0; offset < width; offset += element) {
                uint64_t value = 0, sign = 0;
                memcpy(&value, left + offset, (size_t)element);
                memcpy(&sign, right + offset, (size_t)element);
                uint64_t output = simd_element_negative(sign, element) ? simd_element_negate(value, element)
                                  : sign == 0                          ? 0
                                                                       : value;
                memcpy(result + offset, &output, (size_t)element);
            }
        } else {
            for (int offset = 0; offset < width; offset += 2) {
                int16_t x, y;
                memcpy(&x, left + offset, 2);
                memcpy(&y, right + offset, 2);
                int16_t output = (int16_t)((((x * y) >> 14) + 1) >> 1);
                memcpy(result + offset, &output, 2);
            }
        }
    }
    avx_put(c, instruction->reg, result, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map2_gather(const hl_x86_avx_state *state, struct cpu *c,
                                                         struct insn *instruction, uint64_t next, int map, int op,
                                                         int width) {
    if (map != 2 || op < 0x90 || op > 0x93) return AVX_DISPATCH_UNMATCHED;
    if (instruction->evex) return AVX_DISPATCH_UNIMPLEMENTED;
    int destination = instruction->reg;
    int mask_register = instruction->vvvv;
    if (destination == instruction->m_index || destination == mask_register || mask_register == instruction->m_index)
        avx_undefined();
    int element_size = instruction->vex_w ? 8 : 4;
    int index_size = (op == 0x90 || op == 0x92) ? 4 : 8;
    int lane_count = index_size == 4 ? width / element_size : width / 8;
    int result_bytes = lane_count * element_size;
    uint8_t indices[64], mask[64], output[64];
    avx_get(c, instruction->m_index, indices);
    avx_get(c, mask_register, mask);
    avx_get(c, destination, output);
    uint64_t base = instruction->m_hasbase ? c->r[instruction->m_base] : 0;
    base += (uint64_t)instruction->disp;
    if (instruction->seg == 1)
        base += c->fs_base;
    else if (instruction->seg == 2)
        base += c->gs_base;
    int64_t scale = (int64_t)1 << instruction->m_scale;
    for (int lane = 0; lane < lane_count; lane++) {
        if (mask[(lane + 1) * element_size - 1] & 0x80) {
            int64_t index;
            if (index_size == 4) {
                int32_t narrow_index;
                memcpy(&narrow_index, indices + lane * 4, 4);
                index = narrow_index;
            } else {
                memcpy(&index, indices + lane * 8, 8);
            }
            uint64_t address = hl_x86_avx_address(state, base + (uint64_t)(index * scale));
            if (!avx_try_read(state, address, output + lane * element_size, (size_t)element_size)) {
                avx_put(c, destination, output, 64);
                avx_put(c, mask_register, mask, 64);
                avx_abandon(address, (uint64_t)element_size, X86_SOFT_READ);
            }
        }
        memset(mask + lane * element_size, 0, (size_t)element_size);
    }
    avx_put(c, destination, output, result_bytes);
    uint8_t zero[64] = {0};
    avx_put(c, mask_register, zero, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_map3_byte_immediate(const hl_x86_avx_state *state, struct cpu *c,
                                                                 struct insn *instruction, uint64_t next, int map,
                                                                 int width) {
    if (map != 3 || (instruction->op != 0x0F && instruction->op != 0x42)) return AVX_DISPATCH_UNMATCHED;
    uint8_t left[64], right[64], output[64];
    avx_get(c, instruction->vvvv, left);
    avx_get_rm(state, c, instruction, next, width, right);
    for (int lane = 0; lane < width; lane += 16) {
        if (instruction->op == 0x0F) {
            uint8_t concatenated[32];
            memcpy(concatenated, right + lane, 16);
            memcpy(concatenated + 16, left + lane, 16);
            int shift = (uint8_t)instruction->imm;
            for (int byte = 0; byte < 16; byte++)
                output[lane + byte] = shift < 32 - byte ? concatenated[shift + byte] : 0;
        } else {
            int control = (instruction->imm >> ((lane / 16) * 3)) & 7;
            int right_offset = (control & 3) * 4;
            int left_offset = ((control >> 2) & 1) * 4;
            uint16_t sums[8];
            for (int byte = 0; byte < 8; byte++) {
                int sum = 0;
                for (int component = 0; component < 4; component++) {
                    int difference =
                        (int)left[lane + left_offset + byte + component] - (int)right[lane + right_offset + component];
                    sum += difference < 0 ? -difference : difference;
                }
                sums[byte] = (uint16_t)sum;
            }
            memcpy(output + lane, sums, 16);
        }
    }
    avx_put(c, instruction->reg, output, width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

// Arm the abandon pad, then emulate. A rejected guest access longjmps back here with *c already carrying
// R_SOFTMISS (or R_TRAP for #UD) and cpu->rip left on the instruction.

enum avx_dispatch_result avx_dispatch_vector(const hl_x86_avx_state *state, struct cpu *c, struct insn *instruction,
                                             uint64_t next, int map, int op, int width) {
    if (map == 1 &&
        avx_dispatch_map1_packed_integer_arithmetic(state, c, instruction, next, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map1_horizontal_floating(state, c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map1_scalar_shift(state, c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map1_immediate_shift(c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_packed_numeric_conversion(state, c, instruction, next, map, op, instruction->reg, width) ==
        AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map1_immediate_shuffle(state, c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map2_horizontal_integer(state, c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map2_minimum_maximum(state, c, instruction, next, map, op, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map2_minimum_position(state, c, instruction, next, map, op) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    if (avx_dispatch_map2_ssse3_arithmetic(state, c, instruction, next, map, width) == AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    enum avx_dispatch_result gather = avx_dispatch_map2_gather(state, c, instruction, next, map, op, width);
    if (gather != AVX_DISPATCH_UNMATCHED) return gather;
    return avx_dispatch_map3_byte_immediate(state, c, instruction, next, map, width);
}
