/* VK SYNC DEMO — vk_event: vkCmdSetEvent + vkCmdWaitEvents sequences a dependent compute AFTER a producer
 * compute inside one command buffer, and the dependent op reads the produced data BIT-EXACT.
 *
 * A real C Vulkan program (REAL Khronos loader + OUR ICD → IR → WgpuExecutor/lavapipe). Three storage
 * buffers: A (input, host-filled A[i]=i+1), P (producer output), C (consumer output). ONE command buffer
 * records: producer dispatch P[i] = A[i]*2 (= 2i+2); vkCmdSetEvent(ev); vkCmdWaitEvents(ev); consumer
 * dispatch C[i] = P[i] + 7 (= 2i+9), reading the producer's P. After the (synchronous) submit the guest
 * proves the recorded device set resolved (vkGetEventStatus == VK_EVENT_SET), that host vkResetEvent /
 * vkSetEvent flip the status, and that a bad/never-created VkEvent returns a real VkResult error — never a
 * fake success. The Rust test reads both P and C back off the executor and asserts each is bit-exact,
 * proving the event ordered the dependent read after the producer's write. Prints "VK_EVENT_OK". */
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
                            .pApplicationName = "hl-vk-event", .apiVersion = VK_API_VERSION_1_1};
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
    PFN_vkQueueWaitIdle pWaitIdle = DPROC(dev, vkQueueWaitIdle);
    PFN_vkCreateEvent pCreateEvent = DPROC(dev, vkCreateEvent);
    PFN_vkCmdSetEvent pCmdSetEvent = DPROC(dev, vkCmdSetEvent);
    PFN_vkCmdWaitEvents pCmdWaitEvents = DPROC(dev, vkCmdWaitEvents);
    PFN_vkGetEventStatus pGetEventStatus = DPROC(dev, vkGetEventStatus);
    PFN_vkSetEvent pSetEvent = DPROC(dev, vkSetEvent);
    PFN_vkResetEvent pResetEvent = DPROC(dev, vkResetEvent);
    if (!pCreateEvent || !pCmdSetEvent || !pCmdWaitEvents || !pGetEventStatus || !pSetEvent || !pResetEvent) {
        fprintf(stderr, "event entry points not resolvable\n"); return 6;
    }

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const VkDeviceSize bytes = (VkDeviceSize)N * sizeof(uint32_t);
    VkBuffer bufA, bufP, bufC;
    VkDeviceMemory memA, memP, memC;
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, &bufA, &memA)) { fprintf(stderr, "buf A\n"); return 3; }
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT, &bufP, &memP)) { fprintf(stderr, "buf P\n"); return 3; }
    if (!make_buffer(bytes, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT, &bufC, &memC)) { fprintf(stderr, "buf C\n"); return 3; }

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

    VkDescriptorBufferInfo dbi[4] = {
        {.buffer = bufA, .offset = 0, .range = bytes}, {.buffer = bufP, .offset = 0, .range = bytes},
        {.buffer = bufP, .offset = 0, .range = bytes}, {.buffer = bufC, .offset = 0, .range = bytes}};
    VkWriteDescriptorSet writes[4];
    for (int k = 0; k < 4; k++)
        writes[k] = (VkWriteDescriptorSet){.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
            .dstSet = (k < 2) ? dsetProd : dsetCons, .dstBinding = (uint32_t)(k % 2), .dstArrayElement = 0,
            .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi[k]};
    pUpdateDS(dev, 4, writes, 0, NULL);

    /* Event, created unsignaled. */
    VkEventCreateInfo eci = {.sType = VK_STRUCTURE_TYPE_EVENT_CREATE_INFO};
    VkEvent ev;
    VK(pCreateEvent(dev, &eci, NULL, &ev), "vkCreateEvent");

    /* ONE command buffer: producer dispatch, set event, wait event, consumer dispatch. */
    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfi};
    VkCommandPool pool;
    VK(pCreatePool(dev, &poolci, NULL, &pool), "vkCreateCommandPool");
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBuffer cb;
    VK(pAllocCB(dev, &cbai, &cb), "vkAllocateCommandBuffers");
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VK(pBegin(cb, &cbbi), "vkBeginCommandBuffer");
    pBindPipe(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipeP);
    pBindDS(cb, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dsetProd, 0, NULL);
    pDispatch(cb, N / 64, 1, 1);
    pCmdSetEvent(cb, ev, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT);
    pCmdWaitEvents(cb, 1, &ev, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT,
                   0, NULL, 0, NULL, 0, NULL);
    pBindPipe(cb, VK_PIPELINE_BIND_POINT_COMPUTE, pipeC);
    pBindDS(cb, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &dsetCons, 0, NULL);
    pDispatch(cb, N / 64, 1, 1);
    VK(pEnd(cb), "vkEndCommandBuffer");

    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb};
    VK(pSubmit(queue, 1, &si, VK_NULL_HANDLE), "vkQueueSubmit");
    VK(pWaitIdle(queue), "vkQueueWaitIdle");

    /* The recorded device set resolved at (synchronous) submit completion. */
    VkResult st = pGetEventStatus(dev, ev);
    if (st != VK_EVENT_SET) { fprintf(stderr, "FAIL event status %d != VK_EVENT_SET(3)\n", st); return 7; }
    /* Host reset flips it back; host set flips it again. */
    VK(pResetEvent(dev, ev), "vkResetEvent");
    st = pGetEventStatus(dev, ev);
    if (st != VK_EVENT_RESET) { fprintf(stderr, "FAIL after reset %d != VK_EVENT_RESET(4)\n", st); return 8; }
    VK(pSetEvent(dev, ev), "vkSetEvent");
    st = pGetEventStatus(dev, ev);
    if (st != VK_EVENT_SET) { fprintf(stderr, "FAIL after set %d != VK_EVENT_SET(3)\n", st); return 9; }

    /* Bad/never-created event must return a real error (neither VK_EVENT_SET nor VK_EVENT_RESET). */
    VkResult bad = pGetEventStatus(dev, (VkEvent)0xDEADBEEFULL);
    if (bad == VK_EVENT_SET || bad == VK_EVENT_RESET) { fprintf(stderr, "FAIL status(bad) fake %d\n", bad); return 10; }
    if (pSetEvent(dev, (VkEvent)0xDEADBEEFULL) == VK_SUCCESS) { fprintf(stderr, "FAIL set(bad) fake OK\n"); return 11; }

    printf("VK_EVENT_OK\n");
    free(pds); free(qfp); free(prod_spv); free(cons_spv);
    return 0;
}
