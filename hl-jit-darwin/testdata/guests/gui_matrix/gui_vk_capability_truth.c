// Vulkan ICD capability-truth probe. Uses the loader<->ICD ABI directly so it needs no Vulkan headers
// or libvulkan. Explicit red gate: dd currently advertises Vulkan 1.3 while most mandatory core calls
// are generated success stubs, and accepts an application version newer than its advertised version.
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

typedef int32_t VkResult;
typedef void *VkInstance;
typedef void *PFN_vkVoidFunction;
typedef PFN_vkVoidFunction (*PFN_vkGetInstanceProcAddr)(VkInstance, const char *);
typedef VkResult (*PFN_vkNegotiate)(uint32_t *);
typedef VkResult (*PFN_vkEnumerateInstanceVersion)(uint32_t *);
typedef VkResult (*PFN_vkCreateInstance)(const void *, const void *, VkInstance *);
typedef void (*PFN_vkDestroyInstance)(VkInstance, const void *);

#define VK_SUCCESS 0
#define VK_ERROR_INCOMPATIBLE_DRIVER (-9)
#define VK_STRUCTURE_TYPE_APPLICATION_INFO 0
#define VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO 1
#define VK_MAKE_API_VERSION(variant, major, minor, patch) \
    ((((uint32_t)(variant)) << 29) | (((uint32_t)(major)) << 22) | \
     (((uint32_t)(minor)) << 12) | ((uint32_t)(patch)))
#define VK_API_VERSION_1_0 VK_MAKE_API_VERSION(0, 1, 0, 0)
#define VK_API_VERSION_1_4 VK_MAKE_API_VERSION(0, 1, 4, 0)
#define VK_VERSION_MAJOR(v) (((v) >> 22) & 0x7fu)
#define VK_VERSION_MINOR(v) (((v) >> 12) & 0x3ffu)
#define VK_VERSION_PATCH(v) ((v) & 0xfffu)

struct VkApplicationInfo {
    int32_t sType;
    const void *pNext;
    const char *pApplicationName;
    uint32_t applicationVersion;
    const char *pEngineName;
    uint32_t engineVersion;
    uint32_t apiVersion;
};

struct VkInstanceCreateInfo {
    int32_t sType;
    const void *pNext;
    uint32_t flags;
    const struct VkApplicationInfo *pApplicationInfo;
    uint32_t enabledLayerCount;
    const char *const *ppEnabledLayerNames;
    uint32_t enabledExtensionCount;
    const char *const *ppEnabledExtensionNames;
};

static int fail(const char *why, int64_t value) {
    printf("gui_vk_capability_truth FAIL %s value=%lld\n", why, (long long)value);
    return 1;
}

static VkResult create_for_version(PFN_vkCreateInstance create, uint32_t api, VkInstance *out) {
    const struct VkApplicationInfo app = {
        VK_STRUCTURE_TYPE_APPLICATION_INFO, NULL, "dd-capability-truth", 1, "dd-tests", 1, api,
    };
    const struct VkInstanceCreateInfo ci = {
        VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, NULL, 0, &app, 0, NULL, 0, NULL,
    };
    *out = NULL;
    return create(&ci, NULL, out);
}

int main(int argc, char **argv) {
    const char *library = argc > 1 ? argv[1] : getenv("HL_VK_ICD_LIBRARY");
    if (!library || !*library) {
        printf("gui_vk_capability_truth SKIP set HL_VK_ICD_LIBRARY=/path/to/libvk_dd.so\n");
        return 77;
    }
    void *handle = dlopen(library, RTLD_NOW | RTLD_LOCAL);
    if (!handle) return fail("dlopen", 0);
    PFN_vkNegotiate negotiate = (PFN_vkNegotiate)dlsym(handle, "vk_icdNegotiateLoaderICDInterfaceVersion");
    PFN_vkGetInstanceProcAddr gipa =
        (PFN_vkGetInstanceProcAddr)dlsym(handle, "vk_icdGetInstanceProcAddr");
    if (!negotiate || !gipa) return fail("icd_entrypoints", 0);

    uint32_t loader_interface = 5;
    if (negotiate(&loader_interface) != VK_SUCCESS || loader_interface > 5)
        return fail("loader_negotiation", loader_interface);
    PFN_vkEnumerateInstanceVersion enumerate =
        (PFN_vkEnumerateInstanceVersion)gipa(NULL, "vkEnumerateInstanceVersion");
    PFN_vkCreateInstance create = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
    if (!enumerate || !create) return fail("global_dispatch", 0);

    uint32_t advertised = 0;
    if (enumerate(&advertised) != VK_SUCCESS) return fail("enumerate_version", advertised);
    // Until every mandatory 1.1+ core command and semantic promotion is implemented, 1.0 is the maximum
    // truthful version. This still does not certify 1.0; the generated mandatory-command census does that.
    if (advertised > VK_API_VERSION_1_0) return fail("overadvertised_core_version", advertised);

    VkInstance instance = NULL;
    VkResult r = create_for_version(create, advertised, &instance);
    if (r != VK_SUCCESS || !instance) return fail("create_advertised_version", r);
    PFN_vkDestroyInstance destroy =
        (PFN_vkDestroyInstance)gipa(instance, "vkDestroyInstance");
    if (!destroy) return fail("destroy_dispatch", 0);
    destroy(instance, NULL);

    // A driver must reject a requested version newer than it advertises. dd currently checks only the
    // major number, so 1.4 is incorrectly accepted even while it advertises 1.3.
    instance = NULL;
    r = create_for_version(create, VK_API_VERSION_1_4, &instance);
    if (r != VK_ERROR_INCOMPATIBLE_DRIVER || instance != NULL)
        return fail("accepted_newer_application_version", r);

    printf("gui_vk_capability_truth PASS loader_if=%u api=%u.%u.%u newer_rejected=1\n",
           loader_interface, VK_VERSION_MAJOR(advertised), VK_VERSION_MINOR(advertised),
           VK_VERSION_PATCH(advertised));
    dlclose(handle);
    return 0;
}
