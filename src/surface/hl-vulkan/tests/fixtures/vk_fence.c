/* VK SYNC DEMO — vk_fence: vkQueueSubmit with a VkFence, vkWaitForFences on the host, then the completed
 * work is read back BIT-EXACT — and vkGetFenceStatus tracks the fence's signaled state exactly.
 *
 * A real C Vulkan program (REAL Khronos loader + OUR ICD → IR → WgpuExecutor/lavapipe). Two storage
 * buffers: A (input, host-filled A[i]=i+1) and C (output). A single compute dispatch computes
 * C[i] = A[i]*4 + 1 (= 4i+5). A fence is created UNSIGNALED; the guest proves vkGetFenceStatus is
 * VK_NOT_READY before the work completes, submits the command buffer signalling the fence, blocks in
 * vkWaitForFences, then proves the fence now reads VK_SUCCESS (signaled), that vkResetFences returns it to
 * VK_NOT_READY, and that a bad/never-created VkFence returns a real VkResult error — never a fake success.
 * The Rust test reads C back off the executor and asserts it is bit-exact, proving the host observed the
 * fence-completed work. Prints "VK_FENCE_OK". */
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
                            .pApplicationName = "hl-vk-fence", .apiVersion = VK_API_VERSION_1_1};
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
    PFN_vkCreateFence pCreateFence = DPROC(dev, vkCreateFence);
    PFN_vkWaitForFences pWaitForFences = DPROC(dev, vkWaitForFences);
    PFN_vkGetFenceStatus pGetFenceStatus = DPROC(dev, vkGetFenceStatus);
    PFN_vkResetFences pResetFences = DPROC(dev, vkResetFences);
    if (!pCreateFence || !pWaitForFences || !pGetFenceStatus || !pResetFences) {
        fprintf(stderr, "fence entry points not resolvable\n"); return 6;
    }

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const VkDeviceSize bytes = (VkDeviceSize)N * sizeof(uint32_t);

    /* A input storage; C output storage + TRANSFER_SRC (host reads it back). */
    VkBuffer bufA, bufC;
    VkDeviceMemory memA, memC;
    VkBufferCreateInfo bciA = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes,
                               .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VK(pCreateBuffer(dev, &bciA, NULL, &bufA), "vkCreateBuffer(A)");
    VkMemoryRequirements mrA; pBufReq(dev, bufA, &mrA);
    VkMemoryAllocateInfo maiA = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mrA.size ? mrA.size : bytes, .memoryTypeIndex = 0};
    VK(pAlloc(dev, &maiA, NULL, &memA), "vkAllocateMemory(A)");
    VK(pBind(dev, bufA, memA, 0), "vkBindBufferMemory(A)");

    VkBufferCreateInfo bciC = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes,
                               .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT, .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VK(pCreateBuffer(dev, &bciC, NULL, &bufC), "vkCreateBuffer(C)");
    VkMemoryRequirements mrC; pBufReq(dev, bufC, &mrC);
    VkMemoryAllocateInfo maiC = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = mrC.size ? mrC.size : bytes, .memoryTypeIndex = 0};
    VK(pAlloc(dev, &maiC, NULL, &memC), "vkAllocateMemory(C)");
    VK(pBind(dev, bufC, memC, 0), "vkBindBufferMemory(C)");

    /* A[i] = i + 1 via host-visible map. */
    uint32_t *mapped = NULL;
    VK(pMap(dev, memA, 0, bytes, 0, (void **)&mapped), "vkMapMemory(A)");
    for (uint32_t i = 0; i < N; i++) mapped[i] = i + 1;
    pUnmap(dev, memA);

    VkDescriptorSetLayoutBinding dslb[2];
    for (int k = 0; k < 2; k++)
        dslb[k] = (VkDescriptorSetLayoutBinding){.binding = (uint32_t)k,
                                                 .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                                                 .descriptorCount = 1, .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT};
    VkDescriptorSetLayoutCreateInfo dslci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
                                             .bindingCount = 2, .pBindings = dslb};
    VkDescriptorSetLayout dsl;
    VK(pCreateDSL(dev, &dslci, NULL, &dsl), "vkCreateDescriptorSetLayout");
    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
                                       .setLayoutCount = 1, .pSetLayouts = &dsl};
    VkPipelineLayout layout;
    VK(pCreatePL(dev, &plci, NULL, &layout), "vkCreatePipelineLayout");

    VkShaderModuleCreateInfo smci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = cs_bytes, .pCode = cs_spv};
    VkShaderModule sm;
    VK(pCreateSM(dev, &smci, NULL, &sm), "vkCreateShaderModule");
    VkComputePipelineCreateInfo cpci = {.sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                  .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = sm, .pName = "cs_main"}, .layout = layout};
    VkPipeline pipe;
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpci, NULL, &pipe), "vkCreateComputePipelines");

    VkDescriptorPoolSize dps = {.type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 2};
    VkDescriptorPoolCreateInfo dpci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
                                       .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &dps};
    VkDescriptorPool dpool;
    VK(pCreateDP(dev, &dpci, NULL, &dpool), "vkCreateDescriptorPool");
    VkDescriptorSetAllocateInfo dsai = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
                                        .descriptorPool = dpool, .descriptorSetCount = 1, .pSetLayouts = &dsl};
    VkDescriptorSet dset;
    VK(pAllocDS(dev, &dsai, &dset), "vkAllocateDescriptorSets");
    VkBuffer bufs[2] = {bufA, bufC};
    VkDescriptorBufferInfo dbi[2];
    VkWriteDescriptorSet writes[2];
    for (int k = 0; k < 2; k++) {
        dbi[k] = (VkDescriptorBufferInfo){.buffer = bufs[k], .offset = 0, .range = bytes};
        writes[k] = (VkWriteDescriptorSet){.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            .dstSet = dset, .dstBinding = (uint32_t)k, .dstArrayElement = 0, .descriptorCount = 1,
            .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi[k]};
    }
    pUpdateDS(dev, 2, writes, 0, NULL);

    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfi};
    VkCommandPool pool;
    VK(pCreatePool(dev, &poolci, NULL, &pool), "vkCreateCommandPool");
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cb;
    VK(pAllocCB(dev, &cbai, &cb), "vkAllocateCommandBuffers");
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VK(pBegin(cb, &cbbi), "vkBeginCommandBuffer");
    pBindPipe(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipe);
    pBindDS(cb, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dset, 0, NULL);
    pDispatch(cb, N / 64, 1, 1);
    VK(pEnd(cb), "vkEndCommandBuffer");

    /* Fence, created UNSIGNALED. Before submit it must poll VK_NOT_READY. */
    VkFenceCreateInfo fci = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    VK(pCreateFence(dev, &fci, NULL, &fence), "vkCreateFence");
    VkResult pre = pGetFenceStatus(dev, fence);
    if (pre != VK_NOT_READY) { fprintf(stderr, "FAIL pre-submit fence status %d != VK_NOT_READY(1)\n", pre); return 7; }

    /* Submit signalling the fence, then block for it on the host. */
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK(pSubmit(queue, 1, &si, fence), "vkQueueSubmit");
    VK(pWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX), "vkWaitForFences");

    /* After the wait the fence reflects signaled (VK_SUCCESS). */
    VkResult post = pGetFenceStatus(dev, fence);
    if (post != VK_SUCCESS) { fprintf(stderr, "FAIL post-wait fence status %d != VK_SUCCESS(0)\n", post); return 8; }
    /* vkResetFences returns it to unsignaled. */
    VK(pResetFences(dev, 1, &fence), "vkResetFences");
    VkResult reset = pGetFenceStatus(dev, fence);
    if (reset != VK_NOT_READY) { fprintf(stderr, "FAIL post-reset fence status %d != VK_NOT_READY(1)\n", reset); return 9; }

    /* Bad/never-created fence must return a real error, never a fake success. */
    VkFence bad = (VkFence)0xDEADBEEFULL;
    if (pGetFenceStatus(dev, bad) == VK_SUCCESS) { fprintf(stderr, "FAIL status(bad) fake OK\n"); return 10; }
    if (pWaitForFences(dev, 1, &bad, VK_TRUE, 0) == VK_SUCCESS) { fprintf(stderr, "FAIL wait(bad) fake OK\n"); return 11; }

    printf("VK_FENCE_OK\n");
    free(pds); free(qfp); free(cs_spv);
    return 0;
}
