/* Struct/union size, alignment, and member offsets. The SysV rules for scalar alignment and padding
   are identical between aarch64 and x86_64, so offsetof/alignof/sizeof are arch-neutral. Also probes
   flexible array members, nested aggregates, and #pragma pack. */
#include <stdio.h>
#include <stddef.h>
#include <stdint.h>

struct s1 { char a; int b; char c; double d; short e; };
struct nested { struct s1 inner; char tag; long v; };
union u1 { double d; uint64_t u; struct { uint32_t lo, hi; } parts; char bytes[8]; };
#pragma pack(push, 1)
struct spacked { char a; int b; char c; double d; short e; };
#pragma pack(pop)
struct flex { uint32_t n; char data[]; };

int main(void) {
    printf("s1 size=%zu align=%zu off(b)=%zu off(c)=%zu off(d)=%zu off(e)=%zu\n",
           sizeof(struct s1), _Alignof(struct s1),
           offsetof(struct s1, b), offsetof(struct s1, c),
           offsetof(struct s1, d), offsetof(struct s1, e));
    printf("nested size=%zu align=%zu off(tag)=%zu off(v)=%zu\n",
           sizeof(struct nested), _Alignof(struct nested),
           offsetof(struct nested, tag), offsetof(struct nested, v));
    printf("union size=%zu align=%zu off(hi)=%zu\n",
           sizeof(union u1), _Alignof(union u1), offsetof(union u1, parts.hi));
    printf("packed size=%zu align=%zu off(d)=%zu off(e)=%zu\n",
           sizeof(struct spacked), _Alignof(struct spacked),
           offsetof(struct spacked, d), offsetof(struct spacked, e));
    printf("flex size=%zu off(data)=%zu\n", sizeof(struct flex), offsetof(struct flex, data));
    printf("scalars c=%zu s=%zu i=%zu l=%zu ll=%zu p=%zu\n",
           sizeof(char), sizeof(short), sizeof(int), sizeof(long),
           sizeof(long long), sizeof(void *));
    return 0;
}
