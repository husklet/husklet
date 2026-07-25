/* VK SYNC DEMO — vk_timeline_semaphore: a VK_KHR_timeline_semaphore orders a CONSUMER submit strictly
 * after a PRODUCER submit, and the consumer reads the producer's buffer BIT-EXACT.
 *
 * A real C Vulkan program (REAL Khronos loader + OUR ICD → IR → WgpuExecutor/lavapipe). It creates a
 * TIMELINE VkSemaphore (initial 0) and three storage buffers: A (input, host-filled A[i]=i+1), P (the
 * producer's output), C (the consumer's output). The PRODUCER command buffer dispatches a compute shader
 * P[i] = A[i]*10 - 5 (= 10i+5) and is submitted with a VkTimelineSemaphoreSubmitInfo signalling the
 * semaphore to value 7. The guest then proves the queue-side signal advanced the counter
 * (vkGetSemaphoreCounterValue == 7), that a satisfied wait returns VK_SUCCESS (vkWaitSemaphores >= 7) and
 * an unmet one truthfully times out (>= 8 → VK_TIMEOUT), and that bad/never-created handles return real
 * VkResult errors — never a fake VK_SUCCESS. The CONSUMER command buffer waits on the timeline (>= 7) and
 * dispatches C[i] = P[i] + 1000 (= 10i+1005), reading the producer's P. The Rust test reads both P and C
 * back off the executor and asserts BOTH are bit-exact — proving the ordering point carried the producer's
 * result to the consumer. Prints "VK_TIMELINE_SEMAPHORE_OK". */
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

/* Globals set once in main so the small helpers below stay terse. */
static VkDevice DEV;
static PFN_vkCreateBuffer pCreateBuffer;
static PFN_vkGetBufferMemoryRequirements pBufReq;
static PFN_vkAllocateMemory pAlloc;
static PFN_vkBindBufferMemory pBind;

static int make_buffer(VkDeviceSize bytes, VkBufferUsageFlags usage, VkBuffer *buf, VkDeviceMemory *mem) {
    VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = bytes,
                              .usage = usage, .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    if (pCreateBuffer(DEV, &bci, NULL, buf) != VK_SUCCESS) return 0;
    VkMemoryRequirements mr;
    pBufReq(DEV, *buf, &mr);
    VkMemoryAllocateInfo mai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                               .allocationSize = mr.size ? mr.size : bytes, .memoryTypeIndex = 0};
    if (pAlloc(DEV, &mai, NULL, mem) != VK_SUCCESS) return 0;
    if (pBind(DEV, *buf, *mem, 0) != VK_SUCCESS) return 0;
    return 1;
}

int main(void) {
    setbuf(stdout, NULL);
    signal(SIGALRM, on_timeout);
    alarm(20);

    const char *prod_path = getenv("HL_VK_PROD_SPV");
    const char *cons_path = getenv("HL_VK_CONS_SPV");
    if (!prod_path || !cons_path) { fprintf(stderr, "HL_VK_PROD_SPV / HL_VK_CONS_SPV not set\n"); return 1; }
    size_t prod_bytes = 0, cons_bytes = 0;
    uint32_t *prod_spv = read_spv(prod_path, &prod_bytes);
    uint32_t *cons_spv = read_spv(cons_path, &cons_bytes);
    if (!prod_spv || !cons_spv) return 1;

    void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!loader) { fprintf(stderr, "dlopen(libvulkan.so.1) failed: %s\n", dlerror()); return 1; }
    GIPA = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
    if (!GIPA) { fprintf(stderr, "no vkGetInstanceProcAddr in loader\n"); return 1; }

    PFN_vkCreateInstance pCreateInstance = IPROC(NULL, vkCreateInstance);
    VkApplicationInfo ai = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .pApplicationName = "hl-vk-timeline-semaphore", .apiVersion = VK_API_VERSION_1_2};
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
    /* Faithful real-Vulkan usage: enable the timeline-semaphore feature via the device pNext chain. */
    VkPhysicalDeviceTimelineSemaphoreFeatures tsf = {
        .sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES,
        .timelineSemaphore = VK_TRUE};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .pNext = &tsf,
                              .queueCreateInfoCount = 1, .pQueueCreateInfos = &dqci};
    VkDevice dev;
    VK(pCreateDevice(pd, &dci, NULL, &dev), "vkCreateDevice");
    DEV = dev;

    GDPA = IPROC(inst, vkGetDeviceProcAddr);
    PFN_vkGetDeviceQueue pGetQueue = DPROC(dev, vkGetDeviceQueue);
    pCreateBuffer = DPROC(dev, vkCreateBuffer);
    pBufReq = DPROC(dev, vkGetBufferMemoryRequirements);
    pAlloc = DPROC(dev, vkAllocateMemory);
    pBind = DPROC(dev, vkBindBufferMemory);
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
    PFN_vkCreateSemaphore pCreateSem = DPROC(dev, vkCreateSemaphore);
    PFN_vkGetSemaphoreCounterValue pGetCounter = DPROC(dev, vkGetSemaphoreCounterValue);
    PFN_vkWaitSemaphores pWaitSem = DPROC(dev, vkWaitSemaphores);
    PFN_vkSignalSemaphore pSignalSem = DPROC(dev, vkSignalSemaphore);
    if (!pGetCounter || !pWaitSem || !pSignalSem || !pCreateSem) {
        fprintf(stderr, "timeline semaphore entry points not resolvable\n"); return 6;
    }

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const VkDeviceSize bytes = (VkDeviceSize)N * sizeof(uint32_t);

    /* A input, P producer output, C consumer output. All STORAGE; P/C also TRANSFER_SRC (host reads back). */
    VkBuffer bufA, bufP, bufC;
    VkDeviceMemory memA, memP, memC;
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, &bufA, &memA)) { fprintf(stderr, "buf A\n"); return 3; }
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT, &bufP, &memP)) { fprintf(stderr, "buf P\n"); return 3; }
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT, &bufC, &memC)) { fprintf(stderr, "buf C\n"); return 3; }

    /* A[i] = i + 1 via host-visible map. */
    uint32_t *mapped = NULL;
    VK(pMap(dev, memA, 0, bytes, 0, (void **)&mapped), "vkMapMemory(A)");
    for (uint32_t i = 0; i < N; i++) mapped[i] = i + 1;
    pUnmap(dev, memA);

    /* One shared descriptor-set layout: binding 0 read storage, binding 1 read_write storage. */
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

    /* Two shader modules + two compute pipelines. */
    VkShaderModuleCreateInfo psmci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = prod_bytes, .pCode = prod_spv};
    VkShaderModuleCreateInfo csmci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO, .codeSize = cons_bytes, .pCode = cons_spv};
    VkShaderModule smP, smC;
    VK(pCreateSM(dev, &psmci, NULL, &smP), "vkCreateShaderModule(P)");
    VK(pCreateSM(dev, &csmci, NULL, &smC), "vkCreateShaderModule(C)");
    VkComputePipelineCreateInfo cpciP = {.sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
                  .stage = VK_SHADER_STAGE_COMPUTE_BIT, .module = smP, .pName = "cs_main"}, .layout = layout};
    VkComputePipelineCreateInfo cpciC = cpciP;
    cpciC.stage.module = smC;
    VkPipeline pipeP, pipeC;
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpciP, NULL, &pipeP), "vkCreateComputePipelines(P)");
    VK(pCreateCP(dev, VK_NULL_HANDLE, 1, &cpciC, NULL, &pipeC), "vkCreateComputePipelines(C)");

    /* Descriptor pool with room for 2 sets * 2 storage buffers. */
    VkDescriptorPoolSize dps = {.type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .descriptorCount = 4};
    VkDescriptorPoolCreateInfo dpci = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
                                       .maxSets = 2, .poolSizeCount = 1, .pPoolSizes = &dps};
    VkDescriptorPool dpool;
    VK(pCreateDP(dev, &dpci, NULL, &dpool), "vkCreateDescriptorPool");
    VkDescriptorSetLayout layouts2[2] = {dsl, dsl};
    VkDescriptorSetAllocateInfo dsai = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
                                        .descriptorPool = dpool, .descriptorSetCount = 2, .pSetLayouts = layouts2};
    VkDescriptorSet dsets[2];
    VK(pAllocDS(dev, &dsai, dsets), "vkAllocateDescriptorSets");
    VkDescriptorSet dsetProd = dsets[0], dsetCons = dsets[1];

    /* Producer set: (A read, P write). Consumer set: (P read, C write). */
    VkDescriptorBufferInfo dbi[4] = {
        {.buffer = bufA, .offset = 0, .range = bytes}, {.buffer = bufP, .offset = 0, .range = bytes},
        {.buffer = bufP, .offset = 0, .range = bytes}, {.buffer = bufC, .offset = 0, .range = bytes}};
    VkWriteDescriptorSet writes[4];
    for (int k = 0; k < 4; k++)
        writes[k] = (VkWriteDescriptorSet){.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            .dstSet = (k < 2) ? dsetProd : dsetCons, .dstBinding = (uint32_t)(k % 2), .dstArrayElement = 0,
            .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi[k]};
    pUpdateDS(dev, 4, writes, 0, NULL);

    /* Two command buffers: producer + consumer. */
    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfi};
    VkCommandPool pool;
    VK(pCreatePool(dev, &poolci, NULL, &pool), "vkCreateCommandPool");
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 2};
    VkCommandBuffer cbs[2];
    VK(pAllocCB(dev, &cbai, cbs), "vkAllocateCommandBuffers");
    VkCommandBuffer cbProd = cbs[0], cbCons = cbs[1];
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};

    VK(pBegin(cbProd, &cbbi), "vkBeginCommandBuffer(P)");
    pBindPipe(cbProd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeP);
    pBindDS(cbProd, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dsetProd, 0, NULL);
    pDispatch(cbProd, N / 64, 1, 1);
    VK(pEnd(cbProd), "vkEndCommandBuffer(P)");

    VK(pBegin(cbCons, &cbbi), "vkBeginCommandBuffer(C)");
    pBindPipe(cbCons, VK_PIPELINE_BIND_POINT_COMPUTE, pipeC);
    pBindDS(cbCons, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dsetCons, 0, NULL);
    pDispatch(cbCons, N / 64, 1, 1);
    VK(pEnd(cbCons), "vkEndCommandBuffer(C)");

    /* Timeline semaphore, initial 0. */
    VkSemaphoreTypeCreateInfo stci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO,
                                      .semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE, .initialValue = 0};
    VkSemaphoreCreateInfo sci = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO, .pNext = &stci};
    VkSemaphore sem;
    VK(pCreateSem(dev, &sci, NULL, &sem), "vkCreateSemaphore(timeline)");

    /* PRODUCER submit: signal the timeline to 7 on queue completion. */
    const uint64_t SIGVAL = 7;
    VkTimelineSemaphoreSubmitInfo tssi_p = {.sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
                                            .signalSemaphoreValueCount = 1, .pSignalSemaphoreValues = &SIGVAL};
    VkSubmitInfo siProd = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .pNext = &tssi_p,
                           .commandBufferCount = 1, .pCommandBuffers = &cbProd,
                           .signalSemaphoreCount = 1, .pSignalSemaphores = &sem};
    VK(pSubmit(queue, 1, &siProd, VK_NULL_HANDLE), "vkQueueSubmit(producer)");

    /* Prove the queue-side signal advanced the timeline counter to exactly 7. */
    uint64_t counter = 0;
    VK(pGetCounter(dev, sem, &counter), "vkGetSemaphoreCounterValue");
    if (counter != SIGVAL) { fprintf(stderr, "FAIL counter %lu != %lu\n", (unsigned long)counter, (unsigned long)SIGVAL); return 7; }

    /* A satisfied host wait (>= 7) returns immediately; an unmet one (>= 8) truthfully TIMES OUT. */
    uint64_t wv_ok = 7, wv_no = 8;
    VkSemaphoreWaitInfo wiOk = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO,
                               .semaphoreCount = 1, .pSemaphores = &sem, .pValues = &wv_ok};
    VK(pWaitSem(dev, &wiOk, 0), "vkWaitSemaphores(>=7)");
    VkSemaphoreWaitInfo wiNo = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_WAIT_INFO,
                               .semaphoreCount = 1, .pSemaphores = &sem, .pValues = &wv_no};
    VkResult rno = pWaitSem(dev, &wiNo, 0);
    if (rno != VK_TIMEOUT) { fprintf(stderr, "FAIL wait(>=8) -> %d (want VK_TIMEOUT)\n", rno); return 8; }

    /* Bad/never-created handles must return real errors, never a fake VK_SUCCESS. */
    uint64_t junk = 0;
    if (pGetCounter(dev, (VkSemaphore)0xDEADBEEFULL, &junk) == VK_SUCCESS) { fprintf(stderr, "FAIL counter(bad) fake OK\n"); return 9; }
    uint64_t sval = 5;
    VkSemaphoreSignalInfo bad_ssi = {.sType = VK_STRUCTURE_TYPE_SEMAPHORE_SIGNAL_INFO,
                                     .semaphore = (VkSemaphore)0xDEADBEEFULL, .value = sval};
    if (pSignalSem(dev, &bad_ssi) == VK_SUCCESS) { fprintf(stderr, "FAIL signal(bad) fake OK\n"); return 10; }

    /* CONSUMER submit: wait on the timeline (>= 7) before reading P. */
    VkTimelineSemaphoreSubmitInfo tssi_c = {.sType = VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO,
                                            .waitSemaphoreValueCount = 1, .pWaitSemaphoreValues = &wv_ok};
    VkPipelineStageFlags waitStage = VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT;
    VkSubmitInfo siCons = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .pNext = &tssi_c,
                           .waitSemaphoreCount = 1, .pWaitSemaphores = &sem, .pWaitDstStageMask = &waitStage,
                           .commandBufferCount = 1, .pCommandBuffers = &cbCons};
    VK(pSubmit(queue, 1, &siCons, VK_NULL_HANDLE), "vkQueueSubmit(consumer)");

    printf("VK_TIMELINE_SEMAPHORE_OK\n");
    free(pds); free(qfp); free(prod_spv); free(cons_spv);
    return 0;
}
