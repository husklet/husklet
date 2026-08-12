#include "source.h"

#include <string.h>

int hl_a64_source_validate(const hl_a64_source *source) {
  uint64_t previous = 0;
  if (source == NULL || source->spans == NULL || source->span_count == 0 ||
      source->span_count > HL_A64_SOURCE_MAX_SPANS)
    return 0;
  for (size_t index = 0; index < source->span_count; index++) {
    const hl_a64_source_span *span = &source->spans[index];
    if (span->bytes == NULL || span->size == 0 ||
        span->guest_first > UINT64_MAX - span->size ||
        span->mapping_incarnation != source->mapping_incarnation ||
        span->instruction_epoch != source->instruction_epoch ||
        (index != 0 && span->guest_first < previous))
      return 0;
    previous = span->guest_first + span->size;
  }
  return 1;
}

static int source_byte(const hl_a64_source *source, uint64_t guest,
                       uint8_t *value) {
  for (size_t index = 0; index < source->span_count; index++) {
    const hl_a64_source_span *span = &source->spans[index];
    if (guest >= span->guest_first && guest - span->guest_first < span->size) {
      *value = span->bytes[guest - span->guest_first];
      return 1;
    }
  }
  return 0;
}

size_t hl_a64_source_available(const hl_a64_source *source, uint64_t pc,
                               size_t limit) {
  size_t count = 0;
  uint64_t instruction = pc;
  uint8_t byte;
  if (!hl_a64_source_validate(source) || limit == 0 ||
      limit > HL_A64_SOURCE_MAX_WORDS || (pc & 3u) != 0)
    return 0;
  while (count < limit && instruction <= UINT64_MAX - 4) {
    size_t index;
    for (index = 0; index < 4; index++)
      if (!source_byte(source, instruction + index, &byte))
        break;
    if (index != 4)
      break;
    count++;
    instruction += 4;
  }
  return count;
}

int hl_a64_source_fetch(const hl_a64_source *source, uint64_t pc, size_t count,
                        hl_a64_fetch_result *output) {
  if (output == NULL)
    return 0;
  memset(output, 0, sizeof(*output));
  output->fault_pc = pc;
  if (!hl_a64_source_validate(source) || count == 0 ||
      count > HL_A64_SOURCE_MAX_WORDS || (pc & 3u) != 0 ||
      pc > UINT64_MAX - count * 4)
    return 0;
  output->source_first = pc;
  for (size_t word = 0; word < count; word++) {
    uint8_t bytes[4];
    uint64_t word_pc = pc + word * 4;
    output->fault_pc = word_pc;
    for (size_t byte = 0; byte < sizeof(bytes); byte++)
      if (!source_byte(source, word_pc + byte, &bytes[byte]))
        return 0;
    output->words[word] = (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) |
                          ((uint32_t)bytes[2] << 16) |
                          ((uint32_t)bytes[3] << 24);
    output->count++;
  }
  output->source_last = pc + count * 4;
  output->fault_pc = 0;
  return 1;
}
