#ifndef HL_CORE_CLI_H
#define HL_CORE_CLI_H

typedef enum hl_cli_mode { HL_CLI_GUEST = 0, HL_CLI_CONFIG = 1, HL_CLI_SERVER = 2, HL_CLI_CLIENT = 3 } hl_cli_mode;

typedef struct hl_cli_route {
    hl_cli_mode mode;
    const char *config_path;
} hl_cli_route;

hl_cli_route hl_cli_route_parse(int argc, char *const argv[]);

#endif
