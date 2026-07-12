/* dd-shim-vk dlopen smoke test — the Vulkan analogue of dd-shim-cuda's tests/smoke.c.
 *
 * A plain C program (NOT linked against the ICD, and NOT going through a Vulkan loader) dlopen()s the
 * built libvk_dd.so and drives it exactly the way the Vulkan **loader** would: negotiate the loader<->
 * ICD interface, resolve entry points through `vk_icdGetInstanceProcAddr`, create an instance,
 * enumerate physical devices, and read the device name. This proves the cdylib is a valid ICD drop-in
 * — the private loader<->driver protocol works and the "dd Metal (Vulkan)" device enumerates — even on
 * a host without a Vulkan loader installed.
 *
 *   build+run: cc tests/smoke.c -ldl -o /tmp/vk_smoke && /tmp/vk_smoke <path-to-libvk_dd.so>
 *
 * The ICD/loader ABI mirrored here is ported from Vulkan-Loader docs/LoaderDriverInterface.md +
 * include/vulkan/vk_icd.h (the same source dd-shim-vk/src/icd.rs is ported from).
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef int VkResult; /* VK_SUCCESS == 0 */
typedef void *PFN_vkVoidFunction_holder;
typedef PFN_vkVoidFunction_holder (*PFN_vkGetInstanceProcAddr)(void *instance, const char *name);
typedef VkResult (*PFN_vkNegotiate)(uint32_t *pSupportedVersion);
typedef VkResult (*PFN_vkEnumerateInstanceVersion)(uint32_t *pApiVersion);
typedef VkResult (*PFN_vkCreateInstance)(const void *ci, const void *alloc, void **pInstance);
typedef VkResult (*PFN_vkEnumeratePhysicalDevices)(void *inst, uint32_t *pCount, void **pDevices);
typedef void (*PFN_vkGetPhysicalDeviceProperties)(void *phys, void *pProps);

/* Offsets into VkPhysicalDeviceProperties (Vulkan ABI): apiVersion@0, ... deviceName@ ... We only need
 * deviceName, which begins at offset 24 (u32 apiVersion, u32 driverVersion, u32 vendorID, u32 deviceID,
 * i32 deviceType => 20 bytes; deviceName[256] starts at 24 after alignment). We read a big buffer and
 * scan for the ASCII name to stay layout-robust. */
static const char *find_device_name(const unsigned char *props, size_t len) {
    for (size_t i = 0; i + 4 < len; i++) {
        if (memcmp(props + i, "dd Metal", 8) == 0) {
            return (const char *)(props + i);
        }
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <libvk_dd.so>\n", argv[0]);
        return 2;
    }
    void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        fprintf(stderr, "dlopen failed: %s\n", dlerror());
        return 1;
    }

    PFN_vkNegotiate negotiate = (PFN_vkNegotiate)dlsym(h, "vk_icdNegotiateLoaderICDInterfaceVersion");
    PFN_vkGetInstanceProcAddr icd_gipa =
        (PFN_vkGetInstanceProcAddr)dlsym(h, "vk_icdGetInstanceProcAddr");
    if (!negotiate || !icd_gipa) {
        fprintf(stderr, "dlsym: an ICD entry point is missing\n");
        return 1;
    }

    /* 1) Negotiate the loader<->ICD interface version (loader offers its max; ICD agrees down). */
    uint32_t version = 5;
    VkResult r = negotiate(&version);
    if (r != 0) {
        fprintf(stderr, "negotiate -> %d\n", r);
        return 1;
    }
    printf("negotiated loader<->ICD interface version: %u\n", version);

    /* 2) Resolve global entry points via vk_icdGetInstanceProcAddr(NULL, ...). */
    PFN_vkEnumerateInstanceVersion enumVer =
        (PFN_vkEnumerateInstanceVersion)icd_gipa(NULL, "vkEnumerateInstanceVersion");
    PFN_vkCreateInstance createInstance =
        (PFN_vkCreateInstance)icd_gipa(NULL, "vkCreateInstance");
    if (!enumVer || !createInstance) {
        fprintf(stderr, "icd_gipa: a global entry point is missing\n");
        return 1;
    }

    uint32_t apiVersion = 0;
    r = enumVer(&apiVersion);
    if (r != 0 || apiVersion == 0) {
        fprintf(stderr, "vkEnumerateInstanceVersion -> r=%d api=0x%x\n", r, apiVersion);
        return 1;
    }

    /* 3) Create an instance (NULL VkInstanceCreateInfo is tolerated by our bring-up path). */
    void *instance = NULL;
    r = createInstance(NULL, NULL, &instance);
    if (r != 0 || !instance) {
        fprintf(stderr, "vkCreateInstance -> r=%d inst=%p\n", r, instance);
        return 1;
    }

    /* 4) Resolve instance-level entry points via vk_icdGetInstanceProcAddr(instance, ...). */
    PFN_vkEnumeratePhysicalDevices enumPhys =
        (PFN_vkEnumeratePhysicalDevices)icd_gipa(instance, "vkEnumeratePhysicalDevices");
    PFN_vkGetPhysicalDeviceProperties getProps =
        (PFN_vkGetPhysicalDeviceProperties)icd_gipa(instance, "vkGetPhysicalDeviceProperties");
    if (!enumPhys || !getProps) {
        fprintf(stderr, "icd_gipa: an instance entry point is missing\n");
        return 1;
    }

    uint32_t count = 0;
    r = enumPhys(instance, &count, NULL);
    if (r != 0 || count < 1) {
        fprintf(stderr, "vkEnumeratePhysicalDevices(count) -> r=%d count=%u\n", r, count);
        return 1;
    }
    void *phys[8] = {0};
    if (count > 8) count = 8;
    r = enumPhys(instance, &count, phys);
    if (r != 0 || !phys[0]) {
        fprintf(stderr, "vkEnumeratePhysicalDevices(data) -> r=%d phys0=%p\n", r, phys[0]);
        return 1;
    }

    /* 5) Read the physical-device name and confirm it is the dd device. */
    unsigned char props[1024];
    memset(props, 0, sizeof props);
    getProps(phys[0], props);
    const char *name = find_device_name(props, sizeof props);
    if (!name) {
        fprintf(stderr, "vkGetPhysicalDeviceProperties: dd device name not found\n");
        return 1;
    }

    printf("dlopen ICD smoke OK: api=%u.%u.%u devices=%u dev0=\"%s\" inst=%p\n",
           (apiVersion >> 22) & 0x7f, (apiVersion >> 12) & 0x3ff, apiVersion & 0xfff,
           count, name, instance);
    dlclose(h);
    return 0;
}
