#include "../range.h"

#include <mach/mach.h>
#include <mach/mach_vm.h>
#include <unistd.h>

size_t hl_host_page_size(void) {
    long value = sysconf(_SC_PAGESIZE);
    if (value <= 0 || ((size_t)value & ((size_t)value - 1)) != 0) return 0;
    return (size_t)value;
}

int hl_host_address_mapped(uintptr_t address) {
    mach_vm_address_t region = address;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t info = {0};
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object = MACH_PORT_NULL;
    kern_return_t result = mach_vm_region(mach_task_self(), &region, &size, VM_REGION_BASIC_INFO_64,
                                          (vm_region_info_t)&info, &count, &object);
    if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
    return result == KERN_SUCCESS && address >= (uintptr_t)region && address < (uintptr_t)region + (uintptr_t)size;
}

int hl_host_region_query(uintptr_t address, hl_host_region *region) {
    mach_vm_address_t start = address;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t info = {0};
    mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t object = MACH_PORT_NULL;
    uint32_t protection = 0;
    if (region == NULL) return 0;
    kern_return_t result = mach_vm_region(mach_task_self(), &start, &size, VM_REGION_BASIC_INFO_64,
                                          (vm_region_info_t)&info, &count, &object);
    if (object != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), object);
    if (result != KERN_SUCCESS || start > UINTPTR_MAX || size > SIZE_MAX) return 0;
    if ((info.protection & VM_PROT_READ) != 0) protection |= HL_HOST_REGION_READ;
    if ((info.protection & VM_PROT_WRITE) != 0) protection |= HL_HOST_REGION_WRITE;
    if ((info.protection & VM_PROT_EXECUTE) != 0) protection |= HL_HOST_REGION_EXECUTE;
    region->address = (uintptr_t)start;
    region->size = (size_t)size;
    region->protection = protection;
    return 1;
}
