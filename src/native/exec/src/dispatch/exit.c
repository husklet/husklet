#include "exit.h"

#include <string.h>

hl_native_status hl_native_exit_build(hl_native_exit *output, uint32_t kind, uint32_t access,
                                      uint64_t instruction, uint64_t next, uint64_t address, uint64_t code) {
    if (output == NULL || output->abi != HL_NATIVE_ABI || output->size < sizeof(*output))
        return HL_NATIVE_ARGUMENT;
    switch (kind) {
    case HL_NATIVE_EXIT_BRANCH:
    case HL_NATIVE_EXIT_SYSCALL:
    case HL_NATIVE_EXIT_INTERRUPT:
    case HL_NATIVE_EXIT_EPOCH:
    case HL_NATIVE_EXIT_YIELD:
        if (access != HL_NATIVE_ACCESS_UNKNOWN || address != 0 || code != 0) return HL_NATIVE_ARGUMENT;
        break;
    case HL_NATIVE_EXIT_FALLBACK:
        if (code != 0) return HL_NATIVE_ARGUMENT;
        if (access == HL_NATIVE_ACCESS_UNKNOWN) {
            if (address != 0) return HL_NATIVE_ARGUMENT;
        } else if (access != HL_NATIVE_ACCESS_EXECUTE || address == 0 || address != next) {
            return HL_NATIVE_ARGUMENT;
        }
        break;
    case HL_NATIVE_EXIT_FAULT:
        if (access == HL_NATIVE_ACCESS_UNKNOWN || access > HL_NATIVE_ACCESS_EXECUTE || code == 0)
            return HL_NATIVE_ARGUMENT;
        break;
    case HL_NATIVE_EXIT_FATAL:
        if (access != HL_NATIVE_ACCESS_UNKNOWN || instruction != 0 || next != 0 || address != 0 || code == 0)
            return HL_NATIVE_ARGUMENT;
        break;
    default:
        return HL_NATIVE_ARGUMENT;
    }
    memset(output, 0, sizeof(*output));
    output->abi = HL_NATIVE_ABI;
    output->size = sizeof(*output);
    output->kind = kind;
    output->access = access;
    output->instruction = instruction;
    output->next = next;
    output->address = address;
    output->code = code;
    return HL_NATIVE_OK;
}
