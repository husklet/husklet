#include "ports.h"

#include <string.h>

void hl_linux_ports_init(hl_linux_ports *ports) {
    if (ports != NULL) memset(ports, 0, sizeof *ports);
}

int hl_linux_ports_add(hl_linux_ports *ports, uint16_t host, uint16_t container) {
    return hl_linux_ports_add_address(ports, 0, host, container);
}

int hl_linux_ports_add_address(hl_linux_ports *ports, uint32_t address, uint16_t host, uint16_t container) {
    if (ports == NULL || host == 0 || container == 0 || ports->count >= HL_LINUX_PORT_CAPACITY) return -1;
    ports->entries[ports->count++] = (hl_linux_port_entry){container, host, address};
    return 0;
}

uint32_t hl_linux_ports_address(const hl_linux_ports *ports, uint16_t container) {
    uint32_t index;

    if (ports == NULL) return 0;
    for (index = 0; index < ports->count; ++index)
        if (ports->entries[index].container == container) return ports->entries[index].address;
    return 0;
}

uint16_t hl_linux_ports_host(const hl_linux_ports *ports, uint16_t container) {
    uint32_t index;

    if (ports == NULL) return container;
    for (index = 0; index < ports->count; ++index)
        if (ports->entries[index].container == container) return ports->entries[index].host;
    return container;
}

int hl_linux_ports_contains(const hl_linux_ports *ports, uint16_t container) {
    uint32_t index;

    if (ports == NULL) return 0;
    for (index = 0; index < ports->count; ++index)
        if (ports->entries[index].container == container) return 1;
    return 0;
}

uint32_t hl_linux_ports_count(const hl_linux_ports *ports) {
    return ports != NULL ? ports->count : 0;
}
