#include "cli.h"

#include <stddef.h>
#include <string.h>

hl_cli_route hl_cli_route_parse(int argc, char *const argv[]) {
    hl_cli_route route = {HL_CLI_GUEST, NULL};
    int index;
    if (argv == NULL) return route;
    if (argc > 2 && argv[1] != NULL && strcmp(argv[1], "--configfile") == 0) {
        route.mode = HL_CLI_CONFIG;
        route.config_path = argv[2];
        return route;
    }
    for (index = 1; index < argc; ++index) {
        if (argv[index] == NULL) continue;
        if (strcmp(argv[index], "--server") == 0) {
            route.mode = HL_CLI_SERVER;
            return route;
        }
        if (strcmp(argv[index], "--client") == 0) {
            route.mode = HL_CLI_CLIENT;
            return route;
        }
    }
    return route;
}
