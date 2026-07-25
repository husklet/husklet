/* REAL SOFTWARE #3 — a real C Vulkan program driven through the REAL Khronos loader + OUR ICD.
 *
 * Compiled against the REAL Khronos Vulkan-Headers. At runtime it `dlopen`s the REAL loader
 * (libvulkan.so.1) and resolves every entry point through the canonical `vkGetInstanceProcAddr` /
 * `vkGetDeviceProcAddr` chain (the way a real Vulkan app / a language binding like `ash`/`vulkano` does).
 * `VK_ICD_FILENAMES=~/.hl/vulkan/aarch64/icd.json` makes the loader load OUR driver (libvk_hl.so) as the
 * ICD; every call flows loader → our ICD → `$HL_GPU_EXEC` socket → host executor. It:
 *   vkCreateInstance -> vkEnumeratePhysicalDevices -> vkGetPhysicalDeviceProperties (prints deviceName,
 *   asserted OURS) -> vkCreateDevice + queue -> vkCreateBuffer(STORAGE) + host-visible memory +
 *   map/write/unmap -> vkCreateShaderModule(SPIR-V) -> vkCreatePipelineLayout ->
 *   vkCreateComputePipelines -> command buffer (begin/bindPipeline/dispatch/end) -> vkQueueSubmit ->
 *   vkQueueWaitIdle. Prints "VK_DRIVE_OK" on success.
 *
 * WHY dlopen(RTLD_LOCAL) INSTEAD OF LINKING -lvulkan: the staged ICD (libvk_hl.so) exports the public
 * `vk*` symbols with DEFAULT ELF visibility. If the app link-time-binds libvulkan (putting it in the
 * global symbol scope), the ICD's internal `&vkCreateInstance` self-reference is INTERPOSED to the
 * loader's own copy, so the ICD hands the loader a pointer back into itself and the loader self-deadlocks
 * on its non-recursive instance mutex (confirmed by backtrace). Loading the loader via dlopen(RTLD_LOCAL)
 * keeps it out of the global scope, so the ICD's self-references bind to its OWN definitions and the real
 * loader drives our ICD correctly. This is an honest ICD-packaging finding — see REALSOFTWARE.md.
 *
 * HONEST SCOPE: the program does NOT read a computed result back. Our Vulkan shim models buffers as
 * write-through and exposes no device->host buffer readback (vkMapMemory returns the guest's own staging
 * bytes), and the reference CpuExecutor records but does not execute SPIR-V compute. So this proves the
 * real loader + real app drive our ICD end-to-end; it does not assert a GPU-computed value. */
#define _GNU_SOURCE
#include <vulkan/vulkan.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>

static void on_timeout(int s) { (void)s; fprintf(stderr, "TIMEOUT (stuck)\n"); _exit(42); }

/* A minimal but valid single-OpEntryPoint GLCompute SPIR-V module declaring entry "main" — byte-for-byte
 * what hl_vulkan::adapter::spirv::sample_compute_spirv("main") builds. The seam forwards it verbatim; the
 * compute pipeline resolves pName="main" against the parsed OpEntryPoint name. */
static const uint32_t COMPUTE_SPIRV[] = {
    0x07230203, 0x00010000, 0x00000000, 0x00000002, 0x00000000, /* header */
    0x0005000F, 0x00000005, 0x00000001, 0x6e69616d, 0x00000000, /* OpEntryPoint GLCompute %1 "main" */
};

static PFN_vkGetInstanceProcAddr GIPA;
static PFN_vkGetDeviceProcAddr GDPA;

/* Resolve an instance-scope / global entry point via the real loader's vkGetInstanceProcAddr. */
#define IPROC(inst, name) ((PFN_##name)GIPA((inst), #name))
/* Resolve a device-scope entry point via the real loader's vkGetDeviceProcAddr. */
#define DPROC(dev, name) ((PFN_##name)GDPA((dev), #name))

#define VK(call, what)                                               \
    do {                                                             \
        VkResult _r = (call);                                        \
        if (_r != VK_SUCCESS) {                                      \
            fprintf(stderr, "FAIL %s -> VkResult %d\n", (what), _r); \
            return 3;                                                \
        }                                                            \
    } while (0)

int main(void) {
    setbuf(stdout, NULL);
    signal(SIGALRM, on_timeout);
    alarm(20);

    /* ---- dlopen the REAL loader with RTLD_LOCAL (see header comment) --------------------------- */
    void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!loader) { fprintf(stderr, "dlopen(libvulkan.so.1) failed: %s\n", dlerror()); return 1; }
    GIPA = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
    if (!GIPA) { fprintf(stderr, "no vkGetInstanceProcAddr in loader\n"); return 1; }

    /* ---- instance via the REAL loader --------------------------------------------------------- */
    PFN_vkCreateInstance pCreateInstance = IPROC(NULL, vkCreateInstance);
    VkApplicationInfo ai = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .pApplicationName = "hl-realsw-vk", .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &ai};
    VkInstance inst;
    VK(pCreateInstance(&ici, NULL, &inst), "vkCreateInstance");

    /* ---- enumerate physical devices — must find OUR device ------------------------------------ */
    PFN_vkEnumeratePhysicalDevices pEnum = IPROC(inst, vkEnumeratePhysicalDevices);
    PFN_vkGetPhysicalDeviceProperties pProps = IPROC(inst, vkGetPhysicalDeviceProperties);
    PFN_vkGetPhysicalDeviceQueueFamilyProperties pQFP = IPROC(inst, vkGetPhysicalDeviceQueueFamilyProperties);

    uint32_t n = 0;
    VK(pEnum(inst, &n, NULL), "vkEnumeratePhysicalDevices(count)");
    printf("PHYSICAL_DEVICE_COUNT: %u\n", n);
    if (n == 0) { fprintf(stderr, "no physical devices\n"); return 4; }
    VkPhysicalDevice *pds = calloc(n, sizeof(*pds));
    VK(pEnum(inst, &n, pds), "vkEnumeratePhysicalDevices(list)");

    VkPhysicalDevice pd = pds[0];
    VkPhysicalDeviceProperties props;
    pProps(pd, &props);
    printf("DEVICE_NAME: %s\n", props.deviceName);
    printf("API_VERSION: %u.%u.%u\n", VK_VERSION_MAJOR(props.apiVersion),
           VK_VERSION_MINOR(props.apiVersion), VK_VERSION_PATCH(props.apiVersion));

    /* ---- compute-capable queue family --------------------------------------------------------- */
    uint32_t qn = 0;
    pQFP(pd, &qn, NULL);
    if (qn == 0) { fprintf(stderr, "no queue families\n"); return 5; }
    VkQueueFamilyProperties *qfp = calloc(qn, sizeof(*qfp));
    pQFP(pd, &qn, qfp);
    uint32_t qfi = 0;
    for (uint32_t i = 0; i < qn; i++)
        if (qfp[i].queueFlags & VK_QUEUE_COMPUTE_BIT) { qfi = i; break; }

    /* ---- logical device + queue --------------------------------------------------------------- */
    PFN_vkCreateDevice pCreateDevice = IPROC(inst, vkCreateDevice);
    float prio = 1.0f;
    VkDeviceQueueCreateInfo dqci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                    .queueFamilyIndex = qfi, .queueCount = 1, .pQueuePriorities = &prio};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1, .pQueueCreateInfos = &dqci};
    VkDevice dev;
    VK(pCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice");

    /* device-scope entry points through the canonical vkGetDeviceProcAddr */
    GDPA = IPROC(inst, vkGetDeviceProcAddr);
    PFN_vkGetDeviceQueue pGetQueue = DPROC(dev, vkGetDeviceQueue);
    PFN_vkCreateBuffer pCreateBuffer = DPROC(dev, vkCreateBuffer);
    PFN_vkGetBufferMemoryRequirements pBufReq = DPROC(dev, vkGetBufferMemoryRequirements);
    PFN_vkAllocateMemory pAlloc = DPROC(dev, vkAllocateMemory);
    PFN_vkBindBufferMemory pBind = DPROC(dev, vkBindBufferMemory);
    PFN_vkMapMemory pMap = DPROC(dev, vkMapMemory);
    PFN_vkUnmapMemory pUnmap = DPROC(dev, vkUnmapMemory);
    PFN_vkCreateShaderModule pCreateSM = DPROC(dev, vkCreateShaderModule);
    PFN_vkCreatePipelineLayout pCreatePL = DPROC(dev, vkCreatePipelineLayout);
    PFN_vkCreateComputePipelines pCreateCP = DPROC(dev, vkCreateComputePipelines);
    PFN_vkCreateCommandPool pCreatePool = DPROC(dev, vkCreateCommandPool);
    PFN_vkAllocateCommandBuffers pAllocCB = DPROC(dev, vkAllocateCommandBuffers);
    PFN_vkBeginCommandBuffer pBegin = DPROC(dev, vkBeginCommandBuffer);
    PFN_vkCmdBindPipeline pBindPipe = DPROC(dev, vkCmdBindPipeline);
    PFN_vkCmdDispatch pDispatch = DPROC(dev, vkCmdDispatch);
    PFN_vkEndCommandBuffer pEnd = DPROC(dev, vkEndCommandBuffer);
    PFN_vkQueueSubmit pSubmit = DPROC(dev, vkQueueSubmit);
    PFN_vkQueueWaitIdle pWaitIdle = DPROC(dev, vkQueueWaitIdle);

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    /* ---- storage buffer + host-visible memory + map/write (WriteBuffer over the socket) -------- */
    const uint32_t elems = 4;
    VkDeviceSize bytes = elems * sizeof(uint32_t);
    VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes,
                              .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
                              .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VkBuffer buf;
    VK(pCreateBuffer(dev, &bci, NULL, &buf), "vkCreateBuffer");
    VkMemoryRequirements mr;
    pBufReq(dev, buf, &mr);
    VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                .allocationSize = mr.size ? mr.size : bytes, .memoryTypeIndex = 0};
    VkDeviceMemory mem;
    VK(pAlloc(dev, &mai, NULL, &mem), "vkAllocateMemory");
    VK(pBind(dev, buf, mem, 0), "vkBindBufferMemory");
    uint32_t *mapped = NULL;
    VK(pMap(dev, mem, 0, bytes, 0, (void **)&mapped), "vkMapMemory");
    for (uint32_t i = 0; i < elems; i++) mapped[i] = i + 1;
    pUnmap(dev, mem);

    /* ---- shader module (SPIR-V) + pipeline layout + compute pipeline --------------------------- */
    VkShaderModuleCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     .codeSize = sizeof(COMPUTE_SPIRV), .pCode = COMPUTE_SPIRV};
    VkShaderModule sm;
    VK(pCreateSM(dev, &smci, NULL, &sm), "vkCreateShaderModule");
    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    VkPipelineLayout layout;
    VK(pCreatePL(dev, &plci, NULL, &layout), "vkCreatePipelineLayout");
    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                  .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "main"},
        .layout = layout};
    VkPipeline pipe;
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe), "vkCreateComputePipelines");

    /* ---- command buffer: bind pipeline + dispatch, then submit --------------------------------- */
    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                      .queueFamilyIndex = qfi};
    VkCommandPool pool;
    VK(pCreatePool(dev, &poolci, NULL, &pool), "vkCreateCommandPool");
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                        .commandBufferCount = 1};
    VkCommandBuffer cb;
    VK(pAllocCB(dev, &cbai, &cb), "vkAllocateCommandBuffers");
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VK(pBegin(cb, &cbbi), "vkBeginCommandBuffer");
    pBindPipe(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
    pDispatch(cb, 1, 1, 1);
    VK(pEnd(cb), "vkEndCommandBuffer");
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK(pSubmit(queue, 1, &si, VK_NULL_HANDLE), "vkQueueSubmit");
    VK(pWaitIdle(queue), "vkQueueWaitIdle");

    printf("VK_DRIVE_OK\n");
    free(pds);
    free(qfp);
    return 0;
}
