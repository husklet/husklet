#[cfg(feature = "alloc-count")]
#[global_allocator]
static ALLOCATOR: hl_engine::native::allocations::CountingAllocator = hl_engine::native::allocations::CountingAllocator;

fn main() {
    engine::Worker::run(engine::Guest::X86_64);
}
