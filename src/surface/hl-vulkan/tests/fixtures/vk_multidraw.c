/* VK RENDER-CORRECTNESS DEMO #4 — vk_multidraw: TWO draws in ONE render pass write two DISJOINT regions.
 *
 * A real C Vulkan program (real loader + OUR ICD → IR → lavapipe). Inside a single render pass the program
 * issues `vkCmdDraw(6, 1, 0, 0)` then `vkCmdDraw(6, 1, 6, 0)` against ONE pipeline. The vertex shader's
 * 12-entry position table splits into a LEFT quad (verts 0..5, NDC x∈[-0.9,-0.1], RED) and a RIGHT quad
 * (verts 6..11, x∈[0.1,0.9], BLUE); `firstVertex` selects which quad each draw rasterizes. The gap between
 * them and the frame edges stay the BLACK clear. This proves the executor replays multiple `Enc::Draw`s in
 * one pass, each with its own vertex range, into distinct pixels. Prints "VK_MULTIDRAW_OK". */
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

int main(void) {
    setbuf(stdout, NULL);
    signal(SIGALRM, on_timeout);
    alarm(20);

    const char *vs_path = getenv("HL_VK_VS_SPV");
    const char *fs_path = getenv("HL_VK_FS_SPV");
    if (!vs_path || !fs_path) { fprintf(stderr, "HL_VK_VS_SPV / HL_VK_FS_SPV not set\n"); return 1; }
    size_t vs_bytes = 0, fs_bytes = 0;
    uint32_t *vs_spv = read_spv(vs_path, &vs_bytes);
    uint32_t *fs_spv = read_spv(fs_path, &fs_bytes);
    if (!vs_spv || !fs_spv) return 1;

    void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
    if (!loader) { fprintf(stderr, "dlopen(libvulkan.so.1) failed: %s\n", dlerror()); return 1; }
    GIPA = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
    if (!GIPA) { fprintf(stderr, "no vkGetInstanceProcAddr in loader\n"); return 1; }

    PFN_vkCreateInstance pCreateInstance = IPROC(NULL, vkCreateInstance);
    VkApplicationInfo ai = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .pApplicationName = "hl-vk-multidraw", .apiVersion = VK_API_VERSION_1_1};
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
        if (qfp[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { qfi = i; break; }

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
    PFN_vkCreateImage pCreateImage = DPROC(dev, vkCreateImage);
    PFN_vkGetImageMemoryRequirements pImgReq = DPROC(dev, vkGetImageMemoryRequirements);
    PFN_vkCreateBuffer pCreateBuffer = DPROC(dev, vkCreateBuffer);
    PFN_vkGetBufferMemoryRequirements pBufReq = DPROC(dev, vkGetBufferMemoryRequirements);
    PFN_vkAllocateMemory pAlloc = DPROC(dev, vkAllocateMemory);
    PFN_vkBindImageMemory pBindImg = DPROC(dev, vkBindImageMemory);
    PFN_vkBindBufferMemory pBindBuf = DPROC(dev, vkBindBufferMemory);
    PFN_vkCreateImageView pCreateView = DPROC(dev, vkCreateImageView);
    PFN_vkCreateRenderPass pCreateRP = DPROC(dev, vkCreateRenderPass);
    PFN_vkCreateFramebuffer pCreateFB = DPROC(dev, vkCreateFramebuffer);
    PFN_vkCreateShaderModule pCreateSM = DPROC(dev, vkCreateShaderModule);
    PFN_vkCreatePipelineLayout pCreatePL = DPROC(dev, vkCreatePipelineLayout);
    PFN_vkCreateGraphicsPipelines pCreateGP = DPROC(dev, vkCreateGraphicsPipelines);
    PFN_vkCreateCommandPool pCreatePool = DPROC(dev, vkCreateCommandPool);
    PFN_vkAllocateCommandBuffers pAllocCB = DPROC(dev, vkAllocateCommandBuffers);
    PFN_vkBeginCommandBuffer pBegin = DPROC(dev, vkBeginCommandBuffer);
    PFN_vkCmdBeginRenderPass pCmdBeginRP = DPROC(dev, vkCmdBeginRenderPass);
    PFN_vkCmdBindPipeline pBindPipe = DPROC(dev, vkCmdBindPipeline);
    PFN_vkCmdDraw pCmdDraw = DPROC(dev, vkCmdDraw);
    PFN_vkCmdEndRenderPass pCmdEndRP = DPROC(dev, vkCmdEndRenderPass);
    PFN_vkCmdCopyImageToBuffer pCmdCopyI2B = DPROC(dev, vkCmdCopyImageToBuffer);
    PFN_vkEndCommandBuffer pEnd = DPROC(dev, vkEndCommandBuffer);
    PFN_vkQueueSubmit pSubmit = DPROC(dev, vkQueueSubmit);
    PFN_vkQueueWaitIdle pWaitIdle = DPROC(dev, vkQueueWaitIdle);

    VkQueue queue;
    pGetQueue(dev, qfi, 0, &queue);

    const uint32_t W = 64, H = 64;
    const VkFormat FMT = VK_FORMAT_R8G8B8A8_UNORM;

    VkImageCreateInfo imgci = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                               .imageType = VK_IMAGE_TYPE_2D, .format = FMT,
                               .extent = {W, H, 1}, .mipLevels = 1, .arrayLayers = 1,
                               .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                               .usage = VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT,
                               .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
                               .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED};
    VkImage img;
    VK(pCreateImage(dev, &imgci, NULL, &img), "vkCreateImage");
    VkMemoryRequirements imr;
    pImgReq(dev, img, &imr);
    VkMemoryAllocateInfo imai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                 .allocationSize = imr.size ? imr.size : (VkDeviceSize)W * H * 4,
                                 .memoryTypeIndex = 0};
    VkDeviceMemory imem;
    VK(pAlloc(dev, &imai, NULL, &imem), "vkAllocateMemory(image)");
    VK(pBindImg(dev, img, imem, 0), "vkBindImageMemory");

    VkDeviceSize buf_bytes = (VkDeviceSize)W * H * 4;
    VkBufferCreateInfo bci = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = buf_bytes,
                              .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                              .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    VkBuffer buf;
    VK(pCreateBuffer(dev, &bci, NULL, &buf), "vkCreateBuffer");
    VkMemoryRequirements bmr;
    pBufReq(dev, buf, &bmr);
    VkMemoryAllocateInfo bmai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                 .allocationSize = bmr.size ? bmr.size : buf_bytes, .memoryTypeIndex = 0};
    VkDeviceMemory bmem;
    VK(pAlloc(dev, &bmai, NULL, &bmem), "vkAllocateMemory(buffer)");
    VK(pBindBuf(dev, buf, bmem, 0), "vkBindBufferMemory");

    VkImageViewCreateInfo ivci = {.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO, .image = img,
                                  .viewType = VK_IMAGE_VIEW_TYPE_2D, .format = FMT,
                                  .subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1}};
    VkImageView view;
    VK(pCreateView(dev, &ivci, NULL, &view), "vkCreateImageView");

    VkAttachmentDescription att = {.format = FMT, .samples = VK_SAMPLE_COUNT_1_BIT,
                                   .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR, .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
                                   .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE,
                                   .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
                                   .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
                                   .finalLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL};
    VkAttachmentReference ref = {.attachment = 0, .layout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    VkSubpassDescription sub = {.pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS,
                                .colorAttachmentCount = 1, .pColorAttachments = &ref};
    VkRenderPassCreateInfo rpci = {.sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,
                                   .attachmentCount = 1, .pAttachments = &att,
                                   .subpassCount = 1, .pSubpasses = &sub};
    VkRenderPass rp;
    VK(pCreateRP(dev, &rpci, NULL, &rp), "vkCreateRenderPass");

    VkFramebufferCreateInfo fbci = {.sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO, .renderPass = rp,
                                    .attachmentCount = 1, .pAttachments = &view,
                                    .width = W, .height = H, .layers = 1};
    VkFramebuffer fb;
    VK(pCreateFB(dev, &fbci, NULL, &fb), "vkCreateFramebuffer");

    VkShaderModuleCreateInfo vsci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     .codeSize = vs_bytes, .pCode = vs_spv};
    VkShaderModule vsm;
    VK(pCreateSM(dev, &vsci, NULL, &vsm), "vkCreateShaderModule(vs)");
    VkShaderModuleCreateInfo fsci = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
                                     .codeSize = fs_bytes, .pCode = fs_spv};
    VkShaderModule fsm;
    VK(pCreateSM(dev, &fsci, NULL, &fsm), "vkCreateShaderModule(fs)");

    VkPipelineLayoutCreateInfo plci = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    VkPipelineLayout layout;
    VK(pCreatePL(dev, &plci, NULL, &layout), "vkCreatePipelineLayout");

    VkPipelineShaderStageCreateInfo stages[2] = {
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
         .stage = VK_SHADER_STAGE_VERTEX_BIT, .module = vsm, .pName = "vs_main"},
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
         .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .module = fsm, .pName = "fs_main"}};
    VkPipelineVertexInputStateCreateInfo vin = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    VkPipelineInputAssemblyStateCreateInfo iasm = {.sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
                                                   .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport vp = {0, 0, (float)W, (float)H, 0.0f, 1.0f};
    VkRect2D sc = {{0, 0}, {W, H}};
    VkPipelineViewportStateCreateInfo vps = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
                                             .viewportCount = 1, .pViewports = &vp,
                                             .scissorCount = 1, .pScissors = &sc};
    VkPipelineRasterizationStateCreateInfo rs = {.sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
                                                 .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE,
                                                 .frontFace = VK_FRONT_FACE_COUNTER_CLOCKWISE, .lineWidth = 1.0f};
    VkPipelineMultisampleStateCreateInfo ms = {.sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
                                               .rasterizationSamples = VK_SAMPLE_COUNT_1_BIT};
    VkPipelineColorBlendAttachmentState cba = {.colorWriteMask = 0xF};
    VkPipelineColorBlendStateCreateInfo cb = {.sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
                                              .attachmentCount = 1, .pAttachments = &cba};
    VkGraphicsPipelineCreateInfo gpci = {.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
                                         .stageCount = 2, .pStages = stages,
                                         .pVertexInputState = &vin, .pInputAssemblyState = &iasm,
                                         .pViewportState = &vps, .pRasterizationState = &rs,
                                         .pMultisampleState = &ms, .pColorBlendState = &cb,
                                         .layout = layout, .renderPass = rp, .subpass = 0};
    VkPipeline pipe;
    VK(pCreateGP(dev, VK_NULL_HANDLE, 1, &gpci, NULL, &pipe), "vkCreateGraphicsPipelines");

    VkCommandPoolCreateInfo poolci = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = qfi};
    VkCommandPool pool;
    VK(pCreatePool(dev, &poolci, NULL, &pool), "vkCreateCommandPool");
    VkCommandBufferAllocateInfo cbai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                        .commandBufferCount = 1};
    VkCommandBuffer cb_h;
    VK(pAllocCB(dev, &cbai, &cb_h), "vkAllocateCommandBuffers");
    VkCommandBufferBeginInfo cbbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VK(pBegin(cb_h, &cbbi), "vkBeginCommandBuffer");

    VkClearValue clear;
    clear.color.float32[0] = 0.0f; clear.color.float32[1] = 0.0f;  /* BLACK clear */
    clear.color.float32[2] = 0.0f; clear.color.float32[3] = 1.0f;
    VkRenderPassBeginInfo rpbi = {.sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO, .renderPass = rp,
                                  .framebuffer = fb, .renderArea = {{0, 0}, {W, H}},
                                  .clearValueCount = 1, .pClearValues = &clear};
    pCmdBeginRP(cb_h, &rpbi, VK_SUBPASS_CONTENTS_INLINE);
    pBindPipe(cb_h, VK_PIPELINE_BIND_POINT_GRAPHICS, pipe);
    pCmdDraw(cb_h, 6, 1, 0, 0);   /* draw #1: LEFT quad (verts 0..5, RED) */
    pCmdDraw(cb_h, 6, 1, 6, 0);   /* draw #2: RIGHT quad (verts 6..11, BLUE) */
    pCmdEndRP(cb_h);

    VkBufferImageCopy region = {.bufferOffset = 0, .bufferRowLength = 0, .bufferImageHeight = 0,
                                .imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
                                .imageOffset = {0, 0, 0}, .imageExtent = {W, H, 1}};
    pCmdCopyI2B(cb_h, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &region);

    VK(pEnd(cb_h), "vkEndCommandBuffer");
    VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cb_h};
    VK(pSubmit(queue, 1, &si, VK_NULL_HANDLE), "vkQueueSubmit");
    VK(pWaitIdle(queue), "vkQueueWaitIdle");

    printf("VK_MULTIDRAW_OK\n");
    free(pds); free(qfp); free(vs_spv); free(fs_spv);
    return 0;
}
