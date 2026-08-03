# Vulkan WSI semaphore handoff

The advertised single-queue WSI synchronization path is now modeled end to end.

## Implemented contract

- `vkAcquireNextImageKHR` waits without holding the process-global state lock: zero timeout returns
  `VK_NOT_READY`, finite timeout probes until its deadline, and `UINT64_MAX` waits until availability or
  cancellation by swapchain retirement/device destruction.
- A successful acquire atomically marks the image owned and signals its validated binary semaphore and/or
  fence. Failed and timed-out acquires signal nothing.
- Binary semaphore payloads are stored explicitly. `vkQueueSubmit{,2}` consumes satisfied waits only when
  replay succeeds, rolls them back on refusal, and publishes binary/timeline signals after synchronous
  completion.
- `vkQueuePresentKHR` validates and consumes every binary wait before lowering the presents. An unknown,
  timeline, or unsignaled wait is never treated as satisfied.

Husklet exposes one queue from one queue family. Its host replay is synchronous, so the legal WSI sequence
is already resolved when each next queue operation begins:

1. acquire signals `imageAvailable`;
2. render submission consumes it and signals `renderFinished` after replay;
3. present consumes `renderFinished` before presenting.

MoltenVK's reference is `reference/moltenvk/MoltenVK/MoltenVK/GPUObjects/MVKSwapchain.mm`:
`acquireNextImage()` selects the shortest-availability image and calls
`acquireAndSignalWhenAvailable(semaphore, fence)`. Its queue path retains present wait semaphores until
the presentation operation executes.

If multiple queues are advertised later, queue-owned pending operations become required: a wait may then be
satisfied by a signal submitted later on another queue. Immediate polling must not be extended to claim that
future capability.

## Adjacent validation landed

Swapchain creation now validates the request against the exact formats, present modes, extents, image
counts, transforms, alpha modes, and usage bits returned by the surface queries before allocating presenter
or GPU-surface state. Unsupported create flags and stale `oldSwapchain` handles are rejected. Concurrent
sharing is also rejected truthfully: Vulkan requires more than one unique queue family, while Husklet
currently exposes only queue family 0.

Pool exhaustion never reissues an acquired image. Waiting releases the shim lock between bounded probes,
which lets presentation return an image and lets destruction cancel an infinite wait.

Swapchains retain both the application `VkSurfaceKHR` and the underlying native presentation target
separately from their internal GPU surface. `oldSwapchain` replacement requires the exact application
surface, while native identity independently prevents two active chains from claiming one window. Once a
replacement request is validated, the old chain is
retired before any fallible allocation—even if creation later fails. Images acquired before retirement may
still be presented, but no further acquire is accepted. A second active chain without `oldSwapchain` returns
`VK_ERROR_NATIVE_WINDOW_IN_USE_KHR`. Only FIFO is advertised until MAILBOX and IMMEDIATE have distinct
queueing behavior.
