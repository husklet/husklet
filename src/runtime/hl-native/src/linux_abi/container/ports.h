#ifndef HL_LINUX_ABI_CONTAINER_PORTS_H
#define HL_LINUX_ABI_CONTAINER_PORTS_H

#include <stdint.h>

#define HL_LINUX_PORT_CAPACITY 32

typedef struct hl_linux_port_entry {
    uint16_t container;
    uint16_t host;
    uint32_t address;
} hl_linux_port_entry;

typedef struct hl_linux_ports {
    hl_linux_port_entry entries[HL_LINUX_PORT_CAPACITY];
    uint32_t count;
} hl_linux_ports;

void hl_linux_ports_init(hl_linux_ports *ports);
int hl_linux_ports_add(hl_linux_ports *ports, uint16_t host, uint16_t container);
int hl_linux_ports_add_address(hl_linux_ports *ports, uint32_t address, uint16_t host, uint16_t container);
uint16_t hl_linux_ports_host(const hl_linux_ports *ports, uint16_t container);
uint32_t hl_linux_ports_address(const hl_linux_ports *ports, uint16_t container);
int hl_linux_ports_contains(const hl_linux_ports *ports, uint16_t container);
uint32_t hl_linux_ports_count(const hl_linux_ports *ports);

#endif
