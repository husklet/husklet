#include <unistd.h>

int main(void) {
    char *const argv[] = {
        "/usr/local/bin/python3",
        "-c",
        "d={}\nfor i in range(6000000): d[i%1000]=d.get(i%1000,0)+i\nprint('PYWORK',sum(d.values()))",
        0,
    };
    execv(argv[0], argv);
    return 127;
}
