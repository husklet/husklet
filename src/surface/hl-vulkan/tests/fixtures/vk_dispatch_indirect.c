/* VK COMPUTE DEMO — vk_dispatch_indirect: the compute dispatch dimensions come from a VkBuffer, not the
 * API call. Closes the shim gap where vkCmdDispatchIndirect was a validated no-op.
 *
 * A real C Vulkan program (REAL Khronos loader + OUR ICD → IR → WgpuExecutor/lavapipe). Identical saxpy
 * compute to vk_compute_saxpy (C[i] = A[i]*3 + B[i] over 256 u32 elements, @workgroup_size(64)), except the
 * workgroup counts are NOT passed to the API: they are written into a host-visible
 * VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT buffer as a `VkDispatchIndirectCommand{x=4,y=1,z=1}` and the program
 * issues `vkCmdDispatchIndirect(indbuf, 0)`. The shim reads the {x,y,z} out of the argument buffer and
 * lowers to the SAME `Enc::Dispatch{4,1,1}` the direct `vkCmdDispatch(4,1,1)` would emit — so the result
 * is byte-identical to the direct twin. The Rust test reads C back off the host executor and asserts every
 * element is BIT-EXACT vs the CPU reference. Prints "VK_DISPATCH_INDIRECT_OK". */
#define _GNU_SOURCE
#include <vulkan/vulkan.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#include <unistd.h>

static void on_timeout(int s) { (void)s; fprintf(stderr, "TIMEOUT (stuck)\n"); _exit(42); }

static PFN_vkGetInstanceProcAddr GIPA;
static PFN_vkGetDeviceProcAddr GDPA;
#define IPROC(inst, name) ((PFN_##name)GIPA((inst), #name))
#define DPROC(dev, name) ((PFN_##name)GDPA((dev), #name))

#define VK(call, what)                                               \
    do {                                                             \
        VkResult _r = (call);                                        \
        if (_r != VK_SUCCESS) {                                      \
            fprintf(stderr, "FAIL %s -> VkResult %d\n", (what), _r); \
            return 3;                                                \
        }                                                            \
    } while (0)

static uint32_t *read_spv(const char *path, size_t *out_bytes) {
    FILE *f = fopen(path, "rb");
    if (!f) { fprintf(stderr, "cannot open SPIR-V %s\n", path); return NULL; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (n <= 0 || (n % 4) != 0) { fprintf(stderr, "bad SPIR-V size %ld for %s\n", n, path); fclose(f); return NULL; }
    uint32_t *buf = malloc(n);
    if (fread(buf, 1, n, f) != (size_t)n) { fprintf(stderr, "short read %s\n", path); fclose(f); free(buf); return NULL; }
    fclose(f);
    *out_bytes = (size_t)n;
    return buf;
}

#define N 256

int main(void) {
    setbuf(stdout, NULL);
    signal(SIGALRM, on_timeout);
    alarm(20);

    const char *cs_path = getenv("HL_VK_CS_SPV");
    if (!cs_path) { fprintf(stderr, "HL_VK_CS_SPV not set\n"); return 1; }
    size_t cs_bytes = 0;
    uint32_t *cs_spv = read_spv(cs_path, &cs_bytes);
    if (!cs_spv) return 1;

    void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!loader) { fprintf(stderr, "dlopen(libvulkan.so.1) failed: %s\n", dlerror()); return 1; }
    GIPA = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
    if (!GIPA) { fprintf(stderr, "no vkGetInstanceProcAddr in loader\n"); return 1; }

    PFN_vkCreateInstance pCreateInstance = IPROC(NULL, vkCreateInstance);
    VkApplicationInfo ai = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .pApplicationName = "hl-vk-dispatch-indirect", .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &ai};
    VkInstance inst;
    VK(pCreateInstance(&ici, NULL, &inst), "vkCreateInstance");

    PFN_vkEnumeratePhysicalDevices pEnum = IPROC(inst, vkEnumeratePhysicalDevices);
    PFN_vkGetPhysicalDeviceProperties pProps = IPROC(inst, vkGetPhysicalDeviceProperties);
    PFN_vkGetPhysicalDeviceQueueFamilyProperties pQFP = IPROC(inst, vkGetPhysicalDeviceQueueFamilyProperties);
    uint32_t n = 0;
    VK(pEnum(inst, &n, NULL), "vkEnumeratePhysicalDevices(count)");
    if (n == 0) { fprintf(stderr, "no physical devices\n"); return 4; }
    VkPhysicalDevice *pds = calloc(n, sizeof(*pds));
    VK(pEnum(inst, &n, pds), "vkEnumeratePhysicalDevices(list)");
    VkPhysicalDevice pd = pds[0];
    VkPhysicalDeviceProperties props;
    pProps(pd, &props);
    printf("DEVICE_NAME: %s\n", props.deviceName);

    uint32_t qn = 0;
    pQFP(pd, &qn, NULL);
    if (qn == 0) { fprintf(stderr, "no queue families\n"); return 5; }
    VkQueueFamilyProperties *qfp = calloc(qn, sizeof(*qfp));
    pQFP(pd, &qn, qfp);
    uint32_t qfi = 0;
    for (uint32_t i = 0; i < qn; i++)
        if (qfp[i].queueFlags & VK_QUEUE_COMPUTE_BIT) { qfi = i; break; }

    PFN_vkCreateDevice pCreateDevice = IPROC(inst, vkCreateDevice);
    float prio = 1.0f;
    VkDeviceQueueCreateInfo dqci = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                    .queueFamilyIndex = qfi, .queueCount = 1, .pQueuePriorities = &prio};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1, .pQueueCreateInfos = &dqci};
    VkDevice dev;
    VK(pCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice");

    GDPA = IPROC(inst, vkGetDeviceProcAddr);
    PFN_vkGetDeviceQueue pGetQueue = DPROC(dev, vkGetDeviceQueue);
    PFN_vkCreateBuffer pCreateBuffer = DPROC(dev, vkCreateBuffer);
    PFN_vkGetBufferMemoryRequirements pBufReq = DPROC(dev, vkGetBufferMemoryRequirements);
    PFN_vkAllocateMemory pAlloc = DPROC(dev, vkAllocateMemory);
    PFN_vkBindBufferMemory pBind = DPROC(dev, vkBindBufferMemory);
    PFN_vkMapMemory pMap = DPROC(dev, vkMapMemory);
    PFN_vkUnmapMemory pUnmap = DPROC(dev, vkUnmapMemory);
    PFN_vkCreateShaderModule pCreateSM = DPROC(dev, vkCreateShaderModule);
    PFN_vkCreateDescriptorSetLayout pCreateDSL = DPROC(dev, vkCreateDescriptorSetLayout);
    PFN_vkCreatePipelineLayout pCreatePL = DPROC(dev, vkCreatePipelineLayout);
    PFN_vkCreateDescriptorPool pCreateDP = DPROC(dev, vkCreateDescriptorPool);
    PFN_vkAllocateDescriptorSets pAllocDS = DPROC(dev, vkAllocateDescriptorSets);
    PFN_vkUpdateDescriptorSets pUpdateDS = DPROC(dev, vkUpdateDescriptorSets);
    PFN_vkCreateComputePipelines pCreateCP = DPROC(dev, vkCreateComputePipelines);
    PFN_vkCreateCommandPool pCreatePool = DPROC(dev, vkCreateCommandPool);
    PFN_vkAllocateCommandBuffers pAllocCB = DPROC(dev, vkAllocateCommandBuffers);
    PFN_vkBeginCommandBuffer pBegin = DPROC(dev, vkBeginCommandBuffer);
    PFN_vkCmdBindPipeline pBindPipe = DPROC(dev, vkCmdBindPipeline);
    PFN_vkCmdBindDescriptorSets pBindDS = DPROC(dev, vkCmdBindDescriptorSets);
    PFN_vkCmdDispatchIndirect pDispatchIndirect = DPROC(dev, vkCmdDispatchIndirect);
    PFN_vkEndCommandBuffer pEnd = DPROC(dev, vkEndCommandBuffer);
    PFN_vkQueueSubmit pSubmit = DPROC(dev, vkQueueSubmit);
    PFN_vkQueueWaitIdle pWaitIdle = DPROC(dev, vkQueueWaitIdle);
    if (!pDispatchIndirect) { fprintf(stderr, "vkCmdDispatchIndirect not resolvable\n"); return 6; }

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const VkDeviceSize bytes = (VkDeviceSize)N * sizeof(uint32_t);

    /* ---- three STORAGE buffers: A, B inputs, C output (readable via TRANSFER_SRC) ------------------- */
    VkBuffer bufs[3];
    VkDeviceMemory mems[3];
    const VkBufferUsageFlags usages[3] = {
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
    };
    for (int k = 0; k < 3; k++) {
        VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes,
                                  .usage = usages[k], .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
        VK(pCreateBuffer(dev, &bci, NULL, &bufs[k]), "vkCreateBuffer");
        VkMemoryRequirements mr;
        pBufReq(dev, bufs[k], &mr);
        VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                    .allocationSize = mr.size ? mr.size : bytes, .memoryTypeIndex = 0};
        VK(pAlloc(dev, &mai, NULL, &mems[k]), "vkAllocateMemory");
        VK(pBind(dev, bufs[k], mems[k], 0), "vkBindBufferMemory");
    }
    uint32_t *mapped = NULL;
    VK(pMap(dev, mems[0], 0, bytes, 0, (void **)&mapped), "vkMapMemory(A)");
    for (uint32_t i = 0; i < N; i++) mapped[i] = i + 1;
    pUnmap(dev, mems[0]);
    VK(pMap(dev, mems[1], 0, bytes, 0, (void **)&mapped), "vkMapMemory(B)");
    for (uint32_t i = 0; i < N; i++) mapped[i] = i * 7 + 2;
    pUnmap(dev, mems[1]);

    /* ---- INDIRECT argument buffer: VkDispatchIndirectCommand{x=N/64, y=1, z=1} filled on the CPU ----- */
    VkDeviceSize ind_bytes = sizeof(VkDispatchIndirectCommand);
    VkBufferCreateInfo indci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = ind_bytes,
                                .usage = VK_BUFFER_USAGE_INDIRECT_BUFFER_BIT,
                                .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VkBuffer indbuf;
    VK(pCreateBuffer(dev, &indci, NULL, &indbuf), "vkCreateBuffer(indirect)");
    VkMemoryRequirements indmr;
    pBufReq(dev, indbuf, &indmr);
    VkMemoryAllocateInfo indmai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                   .allocationSize = indmr.size ? indmr.size : ind_bytes, .memoryTypeIndex = 0};
    VkDeviceMemory indmem;
    VK(pAlloc(dev, &indmai, NULL, &indmem), "vkAllocateMemory(indirect)");
    VK(pBind(dev, indbuf, indmem, 0), "vkBindBufferMemory(indirect)");
    void *ind_ptr = NULL;
    VK(pMap(dev, indmem, 0, ind_bytes, 0, &ind_ptr), "vkMapMemory(indirect)");
    VkDispatchIndirectCommand dcmd = {.x = N / 64, .y = 1, .z = 1};
    memcpy(ind_ptr, &dcmd, sizeof(dcmd));
    pUnmap(dev, indmem);

    /* ---- shader module + descriptor set layout (3 STORAGE bindings, COMPUTE) + pipeline layout ------ */
    VkShaderModuleCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     .codeSize = cs_bytes, .pCode = cs_spv};
    VkShaderModule sm;
    VK(pCreateSM(dev, &smci, NULL, &sm), "vkCreateShaderModule");

    VkDescriptorSetLayoutBinding dslb[3];
    for (int k = 0; k < 3; k++) {
        dslb[k] = (VkDescriptorSetLayoutBinding){.binding = (uint32_t)k,
                                                 .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                                 .descriptorCount = 1,
                                                 .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT};
    }
    VkDescriptorSetLayoutCreateInfo dslci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
                                             .bindingCount = 3, .pBindings = dslb};
    VkDescriptorSetLayout dsl;
    VK(pCreateDSL(dev, &dslci, NULL, &dsl), "vkCreateDescriptorSetLayout");

    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
                                       .setLayoutCount = 1, .pSetLayouts = &dsl};
    VkPipelineLayout layout;
    VK(pCreatePL(dev, &plci, NULL, &layout), "vkCreatePipelineLayout");

    VkDescriptorPoolSize dps = {.type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 3};
    VkDescriptorPoolCreateInfo dpci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
                                       .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps};
    VkDescriptorPool dpool;
    VK(pCreateDP(dev, &dpci, NULL, &dpool), "vkCreateDescriptorPool");
    VkDescriptorSetAllocateInfo dsai = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
                                        .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl};
    VkDescriptorSet dset;
    VK(pAllocDS(dev, &dsai, &dset), "vkAllocateDescriptorSets");

    VkDescriptorBufferInfo dbi[3];
    VkWriteDescriptorSet writes[3];
    for (int k = 0; k < 3; k++) {
        dbi[k] = (VkDescriptorBufferInfo){.buffer = bufs[k], .offset = 0, .range = bytes};
        writes[k] = (VkWriteDescriptorSet){.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                                           .dstSet = dset, .dstBinding = (uint32_t)k, .dstArrayElement = 0,
                                           .descriptorCount = 1,
                                           .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                           .pBufferInfo = &dbi[k]};
    }
    pUpdateDS(dev, 3, writes, 0, NULL);

    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                  .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "cs_main"},
        .layout = layout};
    VkPipeline pipe;
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe), "vkCreateComputePipelines");

    /* ---- record: bind pipeline + descriptor set, then INDIRECT dispatch (dims from indbuf) ---------- */
    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfi};
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
    pBindDS(cb, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dset, 0, NULL);
    /* Every workgroup dimension is read from `indbuf` — nothing is passed in the API call. */
    pDispatchIndirect(cb, indbuf, 0);
    VK(pEnd(cb), "vkEndCommandBuffer");
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK(pSubmit(queue, 1, &si, VK_NULL_HANDLE), "vkQueueSubmit");
    VK(pWaitIdle(queue), "vkQueueWaitIdle");

    printf("VK_DISPATCH_INDIRECT_OK\n");
    free(pds); free(qfp); free(cs_spv);
    return 0;
}
