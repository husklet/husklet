/* VK COMPUTE DEMO — vk_compute_shared: a compute shader using WORKGROUP SHARED MEMORY + barrier() runs a
 * per-workgroup tree reduction on lavapipe and the result is read back and asserted BIT-EXACT.
 *
 * A real C Vulkan program (REAL Khronos loader + OUR ICD → IR → WgpuExecutor/lavapipe). Input holds 256
 * u32 (input[i] = i+1) bound as a read-only storage buffer; output holds 4 u32 bound read_write. The
 * compute shader (@workgroup_size(64), 4 workgroups, loaded from $HL_VK_CS_SPV) stages its 64 lane values
 * into a `var<workgroup>` array, `workgroupBarrier()`s, tree-reduces in shared memory (halving stride, a
 * barrier each step), and lane 0 writes the workgroup's sum to output[workgroup_id]. So output[w] =
 * sum_{i in [64w, 64w+64)} (i+1). The Rust test reads output back off the host executor and asserts each of
 * the 4 sums is BIT-EXACT vs the CPU reference — proving shared memory + barriers work end to end through
 * the real Vulkan compute shim. Prints "VK_COMPUTE_SHARED_OK". */
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
#define GROUPS 4

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
                            .pApplicationName = "hl-vk-compute-shared", .apiVersion = VK_API_VERSION_1_1};
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
    PFN_vkCmdDispatch pDispatch = DPROC(dev, vkCmdDispatch);
    PFN_vkEndCommandBuffer pEnd = DPROC(dev, vkEndCommandBuffer);
    PFN_vkQueueSubmit pSubmit = DPROC(dev, vkQueueSubmit);
    PFN_vkQueueWaitIdle pWaitIdle = DPROC(dev, vkQueueWaitIdle);

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const VkDeviceSize in_bytes = (VkDeviceSize)N * sizeof(uint32_t);
    const VkDeviceSize out_bytes = (VkDeviceSize)GROUPS * sizeof(uint32_t);

    /* input: read-only storage (host-visible); output: storage + TRANSFER_SRC so the host reads it back. */
    VkBuffer inbuf, outbuf;
    VkDeviceMemory inmem, outmem;
    VkBufferCreateInfo ibci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = in_bytes,
                               .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
                               .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VK(pCreateBuffer(dev, &ibci, NULL, &inbuf), "vkCreateBuffer(in)");
    VkMemoryRequirements imr;
    pBufReq(dev, inbuf, &imr);
    VkMemoryAllocateInfo imai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                 .allocationSize = imr.size ? imr.size : in_bytes, .memoryTypeIndex = 0};
    VK(pAlloc(dev, &imai, NULL, &inmem), "vkAllocateMemory(in)");
    VK(pBind(dev, inbuf, inmem, 0), "vkBindBufferMemory(in)");

    VkBufferCreateInfo obci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = out_bytes,
                               .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT,
                               .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VK(pCreateBuffer(dev, &obci, NULL, &outbuf), "vkCreateBuffer(out)");
    VkMemoryRequirements omr;
    pBufReq(dev, outbuf, &omr);
    VkMemoryAllocateInfo omai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                 .allocationSize = omr.size ? omr.size : out_bytes, .memoryTypeIndex = 0};
    VK(pAlloc(dev, &omai, NULL, &outmem), "vkAllocateMemory(out)");
    VK(pBind(dev, outbuf, outmem, 0), "vkBindBufferMemory(out)");

    /* input[i] = i + 1 via host-visible map/write/unmap. */
    uint32_t *mapped = NULL;
    VK(pMap(dev, inmem, 0, in_bytes, 0, (void **)&mapped), "vkMapMemory(in)");
    for (uint32_t i = 0; i < N; i++) mapped[i] = i + 1;
    pUnmap(dev, inmem);

    /* ---- shader + descriptor set layout (binding 0 read storage, binding 1 read_write) + layout ------ */
    VkShaderModuleCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     .codeSize = cs_bytes, .pCode = cs_spv};
    VkShaderModule sm;
    VK(pCreateSM(dev, &smci, NULL, &sm), "vkCreateShaderModule");

    VkDescriptorSetLayoutBinding dslb[2];
    for (int k = 0; k < 2; k++) {
        dslb[k] = (VkDescriptorSetLayoutBinding){.binding = (uint32_t)k,
                                                 .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                                 .descriptorCount = 1,
                                                 .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT};
    }
    VkDescriptorSetLayoutCreateInfo dslci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
                                             .bindingCount = 2, .pBindings = dslb};
    VkDescriptorSetLayout dsl;
    VK(pCreateDSL(dev, &dslci, NULL, &dsl), "vkCreateDescriptorSetLayout");

    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
                                       .setLayoutCount = 1, .pSetLayouts = &dsl};
    VkPipelineLayout layout;
    VK(pCreatePL(dev, &plci, NULL, &layout), "vkCreatePipelineLayout");

    VkDescriptorPoolSize dps = {.type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 2};
    VkDescriptorPoolCreateInfo dpci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
                                       .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps};
    VkDescriptorPool dpool;
    VK(pCreateDP(dev, &dpci, NULL, &dpool), "vkCreateDescriptorPool");
    VkDescriptorSetAllocateInfo dsai = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
                                        .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl};
    VkDescriptorSet dset;
    VK(pAllocDS(dev, &dsai, &dset), "vkAllocateDescriptorSets");

    VkBuffer bufs[2] = {inbuf, outbuf};
    VkDeviceSize ranges[2] = {in_bytes, out_bytes};
    VkDescriptorBufferInfo dbi[2];
    VkWriteDescriptorSet writes[2];
    for (int k = 0; k < 2; k++) {
        dbi[k] = (VkDescriptorBufferInfo){.buffer = bufs[k], .offset = 0, .range = ranges[k]};
        writes[k] = (VkWriteDescriptorSet){.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                                           .dstSet = dset, .dstBinding = (uint32_t)k, .dstArrayElement = 0,
                                           .descriptorCount = 1,
                                           .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                           .pBufferInfo = &dbi[k]};
    }
    pUpdateDS(dev, 2, writes, 0, NULL);

    VkComputePipelineCreateInfo cpci = {
        .sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                  .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "cs_main"},
        .layout = layout};
    VkPipeline pipe;
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe), "vkCreateComputePipelines");

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
    pDispatch(cb, GROUPS, 1, 1);
    VK(pEnd(cb), "vkEndCommandBuffer");
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK(pSubmit(queue, 1, &si, VK_NULL_HANDLE), "vkQueueSubmit");
    VK(pWaitIdle(queue), "vkQueueWaitIdle");

    printf("VK_COMPUTE_SHARED_OK\n");
    free(pds); free(qfp); free(cs_spv);
    return 0;
}
