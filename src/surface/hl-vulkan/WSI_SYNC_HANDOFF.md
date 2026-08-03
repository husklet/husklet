# Vulkan WSI semaphore handoff

`vkQueuePresentKHR::pWaitSemaphores` cannot be propagated honestly by the current runtime.

## Missing state and ordering

- `SemaphoreRec` distinguishes binary from timeline semaphores but stores no binary signaled state.
- `vkQueueSubmit` and `vkQueueSubmit2` discard binary wait and signal arrays.
- `vkAcquireNextImageKHR` discards its semaphore and fence arguments.
- `vkQueuePresentKHR` discards `pWaitSemaphores`.
- The guest queue service is synchronous: `sink.submit()` completes before returning. It has no pending
  queue operation that can wait for a semaphore another queue signals later.

Marking every binary semaphore satisfied would fake success. Rejecting every not-yet-signaled wait would
also violate Vulkan, where queue submission may wait asynchronously. A complete implementation needs:

1. Give each binary semaphore an owned payload state (`unsignaled`, `pending signal`, `signaled`) and
   consume it exactly once at a successful wait.
2. Preserve each `VkSubmitInfo` as an ordered queue operation instead of flattening all command buffers.
3. Add queue-owned pending operations so a wait can be satisfied by a later submission on another queue.
4. Signal acquire semaphores/fences only when the image becomes available.
5. Carry present waits into the same queue scheduler; submit `Cmd::Present` only after all waits resolve.
6. Retire or roll back semaphore transitions when GPU submission is refused, using the committed-prefix
   outcome rather than the top-level return alone.

MoltenVK's reference is `reference/moltenvk/MoltenVK/MoltenVK/GPUObjects/MVKSwapchain.mm`:
`acquireNextImage()` selects the shortest-availability image and calls
`acquireAndSignalWhenAvailable(semaphore, fence)`. Its queue path retains present wait semaphores until
the presentation operation executes.

Fail-first coverage should be a two-submit sequence: acquire signals `imageAvailable`; render waits and
consumes it, then signals `renderFinished`; present waits and consumes `renderFinished`. A second wait on
either consumed payload must remain pending. A cross-queue test must signal after the wait was enqueued;
that test prevents replacing the scheduler with immediate guest-side polling.

## Adjacent validation landed

Swapchain creation now validates the request against the exact formats, present modes, extents, image
counts, transforms, alpha modes, and usage bits returned by the surface queries before allocating presenter
or GPU-surface state. Unsupported create flags and stale `oldSwapchain` handles are rejected. Concurrent
sharing is also rejected truthfully: Vulkan requires more than one unique queue family, while Husklet
currently exposes only queue family 0.

This validation is deliberately not presented as semaphore support. The synchronization work above remains
required before acquire, submit, or present wait/signal parameters can be honored.

Swapchains retain both the application `VkSurfaceKHR` and the underlying native presentation target
separately from their internal GPU surface. Replacement follows the native target, so two Vulkan surface
wrappers around one window are compatible. Once a replacement request is validated, the old chain is
retired before any fallible allocation—even if creation later fails. Images acquired before retirement may
still be presented, but no further acquire is accepted. A second active chain without `oldSwapchain` returns
`VK_ERROR_NATIVE_WINDOW_IN_USE_KHR`. Only FIFO is advertised until MAILBOX and IMMEDIATE have distinct
queueing behavior.
