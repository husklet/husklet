// getopt_long parsing of a fixed argv (short, long, optarg, non-option). Portable verdicts.
#include <stdio.h>
#include <getopt.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    char *argv[] = {"prog", "-v", "--name=bob", "-n", "5", "file.txt", NULL};
    int argc = 6;
    static struct option opts[] = {
        {"name", required_argument, 0, 1000},
        {"verbose", no_argument, 0, 'v'},
        {0, 0, 0, 0}
    };
    int verbose = 0; char name[16] = {0}; int num = 0;
    int c, idx;
    optind = 1;
    while ((c = getopt_long(argc, argv, "vn:", opts, &idx)) != -1) {
        if (c == 'v') verbose = 1;
        else if (c == 1000) snprintf(name, sizeof name, "%s", optarg);
        else if (c == 'n') num = atoi(optarg);
    }
    int d1 = verbose == 1;
    int d2 = strcmp(name, "bob") == 0;
    int d3 = num == 5;
    int d4 = optind == 5 && strcmp(argv[optind], "file.txt") == 0;
    printf("getopt d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
