#include <string.h>
#include <unistd.h>

struct variant {
    const char *name;
    const char *code;
};

static const struct variant variants[] = {
    {"integer", "s=0\nfor i in range(10000000): s=(s+i)&0xffffffff\nprint('INTEGER',s)"},
    {"calls", "def f(x): return (x+3)&0xffffffff\ns=0\nfor i in range(9600000): s=f(s)\nprint('CALLS',s)"},
    {"list", "a=list(range(4096));s=0\nfor i in range(10000000): s+=a[i&4095]\nprint('LIST',s)"},
    {"branch", "s=0\nfor i in range(7000000):\n x=i&3\n if x==0:s+=1\n elif x==1:s-=3\n elif x==2:s+=5\n "
               "else:s-=7\nprint('BRANCH',s)"},
    {"dict", "d={}\nfor i in range(6000000): d[i%1000]=d.get(i%1000,0)+i\nprint('DICT',sum(d.values()))"},
    {"bigint", "x=1\nfor i in range(6000000): x=(x*6364136223846793005+i)&((1<<127)-1)\nprint('BIGINT',x)"},
};

int main(int argc, char **argv) {
    if (argc != 2) return 64;
    for (unsigned i = 0; i < sizeof(variants) / sizeof(variants[0]); ++i) {
        if (strcmp(argv[1], variants[i].name) != 0) continue;
        char *const guest[] = {"/usr/local/bin/python3", "-B", "-c", (char *)variants[i].code, 0};
        execv(guest[0], guest);
        return 127;
    }
    return 64;
}
