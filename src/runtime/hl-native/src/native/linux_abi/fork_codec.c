#include "fork_codec.h"

#include <stdint.h>
#include <string.h>

int hl_fork_wire_pack_strings(char *output, size_t capacity, size_t *offset, int count, char *const strings[]) {
    int32_t encoded_count;
    if (output == NULL || offset == NULL || count < 0 || (count > 0 && strings == NULL) || *offset > capacity ||
        capacity - *offset < sizeof encoded_count)
        return -1;
    encoded_count = count;
    memcpy(output + *offset, &encoded_count, sizeof encoded_count);
    *offset += sizeof encoded_count;
    for (int index = 0; index < count; index++) {
        size_t length;
        int32_t encoded_length;
        if (strings[index] == NULL) return -1;
        length = strlen(strings[index]) + 1;
        if (length > INT32_MAX || capacity - *offset < sizeof encoded_length ||
            capacity - *offset - sizeof encoded_length < length)
            return -1;
        encoded_length = (int32_t)length;
        memcpy(output + *offset, &encoded_length, sizeof encoded_length);
        *offset += sizeof encoded_length;
        memcpy(output + *offset, strings[index], length);
        *offset += length;
    }
    return 0;
}

int hl_fork_wire_unpack_strings(const char *input, size_t size, size_t *offset, char **strings, int capacity) {
    int32_t count;
    if (input == NULL || offset == NULL || strings == NULL || capacity < 1 || *offset > size ||
        size - *offset < sizeof count)
        return -1;
    memcpy(&count, input + *offset, sizeof count);
    *offset += sizeof count;
    if (count < 0 || count >= capacity) return -1;
    for (int index = 0; index < count; index++) {
        int32_t length;
        if (size - *offset < sizeof length) return -1;
        memcpy(&length, input + *offset, sizeof length);
        *offset += sizeof length;
        if (length < 1 || (size_t)length > size - *offset || input[*offset + (size_t)length - 1] != 0 ||
            memchr(input + *offset, 0, (size_t)length - 1) != NULL)
            return -1;
        strings[index] = (char *)input + *offset;
        *offset += (size_t)length;
    }
    strings[count] = NULL;
    return count;
}
