#ifndef HL_NATIVE_AARCH64_MEMORY_H
#define HL_NATIVE_AARCH64_MEMORY_H

#include <stdint.h>

#include "../../../include/executor.h"

#define HL_A64_MEMORY_SITE_MAX 4u

typedef struct hl_a64_memory_site {
  uint64_t code_offset;
  int64_t displacement;
  uint32_t access;
  uint32_t width;
} hl_a64_memory_site;

typedef struct hl_a64_memory_sites {
  uint32_t count;
  hl_a64_memory_site entries[HL_A64_MEMORY_SITE_MAX];
} hl_a64_memory_sites;

typedef enum hl_a64_memory_kind {
  HL_A64_MEMORY_NONE,
  HL_A64_MEMORY_LITERAL,
  HL_A64_MEMORY_UNSIGNED,
  HL_A64_MEMORY_UNSCALED,
  HL_A64_MEMORY_REGISTER,
  HL_A64_MEMORY_PAIR,
  HL_A64_MEMORY_EXCLUSIVE,
  HL_A64_MEMORY_ATOMIC,
  HL_A64_MEMORY_STRUCTURE,
  HL_A64_MEMORY_PREFETCH,
} hl_a64_memory_kind;

typedef struct hl_a64_memory {
  hl_a64_memory_kind kind;
  uint64_t pc;
  uint64_t bytes;
  int64_t offset;
  uint32_t word;
  uint8_t base;
  uint8_t target;
  uint8_t target2;
  uint8_t index;
  uint8_t read;
  uint8_t write;
  uint8_t vector;
  uint8_t writeback;
  uint8_t postindex;
  uint8_t stolen;
} hl_a64_memory;

int hl_a64_memory_decode(uint32_t, uint64_t, hl_a64_memory *);

#endif
