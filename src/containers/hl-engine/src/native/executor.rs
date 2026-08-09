#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;

#[cfg(test)]
use hl_execution::ScalarState;
use hl_execution::{Aarch64CpuState, CpuState as X86CpuState, FlagState, Nzcv};
use hl_isa::{AddressRange, GuestAddress};
use hl_memory::{DirectAuthorityLease, ExecutableToken, MemoryAccessHost, MemoryError, ProjectionLease, Protection};

/// Prices a native crossing in host heap allocations rather than in time, which a
/// loaded box cannot invalidate. Off unless the `alloc-count` feature is built.
#[cfg(feature = "alloc-count")]
pub mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering};

    thread_local! {
        static THREAD: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) static CROSSINGS: AtomicU64 = AtomicU64::new(0);
    pub(super) static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    /// How the crossing classified its writes, and what the publish it chose cost.
    pub(super) static WRITES_NONE: AtomicU64 = AtomicU64::new(0);
    pub(super) static WRITES_EXACT: AtomicU64 = AtomicU64::new(0);
    pub(super) static WRITES_FULL: AtomicU64 = AtomicU64::new(0);
    pub(super) static PUBLISH_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn count(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Attributes to `PUBLISH_ALLOCATIONS` everything the calling thread allocates
    /// while `body` runs.
    pub(super) fn in_publish<R>(body: impl FnOnce() -> R) -> R {
        let before = thread_count();
        let value = body();
        PUBLISH_ALLOCATIONS.fetch_add(thread_count().wrapping_sub(before), Ordering::Relaxed);
        value
    }

    pub(super) fn thread_count() -> u64 {
        THREAD.try_with(Cell::get).unwrap_or(0)
    }

    fn bump() {
        let _ = THREAD.try_with(|count| count.set(count.get().wrapping_add(1)));
    }

    /// Counts one crossing and the allocations its thread made inside it.
    pub(super) struct Crossing(u64);

    impl Crossing {
        pub(super) fn begin() -> Self {
            Self(thread_count())
        }
    }

    impl Drop for Crossing {
        fn drop(&mut self) {
            CROSSINGS.fetch_add(1, Ordering::Relaxed);
            ALLOCATIONS.fetch_add(thread_count().wrapping_sub(self.0), Ordering::Relaxed);
        }
    }

    pub struct CountingAllocator;

    // SAFETY: every method forwards to `System` unchanged; the counter is a thread-local
    // `Cell<u64>` with no destructor, so it cannot re-enter the allocator.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            bump();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            bump();
            unsafe { System.realloc(pointer, layout, size) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }
    }
}

const ABI: u32 = 1;
const WRITE_EXACT: u16 = 1;
const AARCH64: u32 = 1;
const X86_64: u32 = 2;

fn projection_permissions(authority: Protection, mapped: Protection) -> Protection {
    authority.union(if mapped.contains(Protection::EXECUTE) {
        Protection::EXECUTE
    } else {
        Protection::NONE
    })
}

/// EXECUTE in a projection view marks bytes a store may modify code through, not an
/// access grant, so a writable alias of an object mapped executable elsewhere carries it.
fn alias_execute<H: MemoryAccessHost>(lease: &ProjectionLease<'_, H>, address: GuestAddress) -> Protection {
    match lease.executable_aliases(address) {
        Ok(evidence) if evidence.present => Protection::EXECUTE,
        _ => Protection::NONE,
    }
}

/// The permission word published for one native projection view.
fn view_permissions<H: MemoryAccessHost>(
    lease: &ProjectionLease<'_, H>,
    mapped: Protection,
    address: GuestAddress,
) -> u32 {
    u32::from(mapped.union(alias_execute(lease, address)).bits())
}

use hl_native::cpu as schema;

#[repr(C)]
struct Handle {
    _opaque: [u8; 0],
}

#[repr(C)]
struct InterruptHandle {
    _opaque: [u8; 0],
}

#[repr(C)]
struct DirectToken {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectDescriptor {
    abi: u32,
    size: u32,
    permissions: u32,
    reserved: u32,
    guest_first: u64,
    guest_last: u64,
    host_first: u64,
    mapping_incarnation: u64,
    mapping_generation: u64,
    instruction_generation: u64,
}

unsafe extern "C" {
    fn hl_native_create(config: *const Config, output: *mut *mut Handle) -> u32;
    fn hl_native_interrupt_create(output: *mut *mut InterruptHandle) -> u32;
    fn hl_native_interrupt_set(token: *mut InterruptHandle, value: u64) -> u32;
    fn hl_native_interrupt_destroy(token: *mut InterruptHandle);
    fn hl_native_changed(executor: *mut Handle, changes: *const Change, count: usize) -> u32;
    fn hl_native_diagnose(executor: *const Handle, output: *mut Diagnostics) -> u32;
    fn hl_native_state_invariant() -> *const std::ffi::c_char;
    #[cfg(test)]
    fn hl_native_fault_scope_contains(scope: *const FaultScope, host_pc: u64) -> i32;
    #[cfg(test)]
    fn hl_native_fault_scope_leave(scope: *mut FaultScope) -> u32;
    fn hl_native_direct_register(
        executor: *mut Handle,
        authority: *const DirectDescriptor,
        output: *mut *mut DirectToken,
    ) -> u32;
    fn hl_native_direct_generation(executor: *const Handle, token: *const DirectToken) -> u64;
    fn hl_native_direct_identity(executor: *const Handle, token: *const DirectToken) -> u64;
    fn hl_native_direct_unregister(executor: *mut Handle, token: *mut DirectToken) -> u32;
    fn hl_native_fault_process_acquire() -> u32;
    fn hl_native_fault_process_release() -> u32;
    fn hl_native_fault_thread_attach() -> u32;
    fn hl_native_fault_thread_publish(scope: *const FaultScope, generation: *mut u64) -> u32;
    fn hl_native_fault_thread_unpublish(scope: *const FaultScope, generation: u64) -> u32;
    fn hl_native_fault_thread_detach() -> u32;
    fn hl_native_destroy(executor: *mut Handle) -> u32;
    fn hl_native_flush(address: *mut c_void, size: usize);
    fn hl_native_run(
        executor: *mut Handle,
        cpu: *mut CpuHandle,
        request: *const RunRequest,
        output: *mut RunExit,
    ) -> u32;
}

/// Rust ownership bridge for a native generation token. The memory lease is
/// retained until Drop retires the token; callers cannot construct either the
/// descriptor or token independently.
pub(crate) struct DirectAuthority<'executor, 'memory, H: MemoryAccessHost> {
    executor: &'executor Executor,
    token: Option<NonNull<DirectToken>>,
    lease: Option<DirectAuthorityLease<'memory, H>>,
}

impl<'memory, H: MemoryAccessHost> DirectAuthority<'_, 'memory, H> {
    fn generation(&self) -> Option<u64> {
        let token = self.token?;
        // SAFETY: the `&'executor Executor` borrow keeps the handle alive, and `token` is
        // a live registration this struct owns until `retire`; the query only reads.
        let generation = unsafe { hl_native_direct_generation(self.executor.handle.as_ptr(), token.as_ptr()) };
        (generation != 0).then_some(generation)
    }

    fn request_identity(&self) -> Option<(*const DirectToken, u64, u64)> {
        let token = self.token?;
        let generation = self.generation()?;
        // SAFETY: same borrow-held handle, and `generation()` above just confirmed the
        // owned token is still registered; the query only reads native state.
        let identity = unsafe { hl_native_direct_identity(self.executor.handle.as_ptr(), token.as_ptr()) };
        (identity != 0).then_some((token.as_ptr(), generation, identity))
    }

    pub(crate) fn into_lease(mut self) -> Result<DirectAuthorityLease<'memory, H>, ()> {
        self.retire()?;
        self.lease.take().ok_or(())
    }

    fn retire(&mut self) -> Result<(), ()> {
        let Some(token) = self.token else { return Ok(()) };
        // SAFETY: `self.token` is taken exactly once — cleared below on success — so this
        // unregisters a live registration once, under the borrow that keeps the handle up.
        if unsafe { hl_native_direct_unregister(self.executor.handle.as_ptr(), token.as_ptr()) } != 0 {
            return Err(());
        }
        self.token = None;
        Ok(())
    }
}

impl<H: MemoryAccessHost> Drop for DirectAuthority<'_, '_, H> {
    fn drop(&mut self) {
        let status = self.retire();
        debug_assert!(status.is_ok(), "direct authority retired during native execution");
    }
}

pub(crate) struct InterruptToken {
    handle: NonNull<InterruptHandle>,
    interrupted: std::sync::atomic::AtomicBool,
}

unsafe impl Send for InterruptToken {}
unsafe impl Sync for InterruptToken {}

impl InterruptToken {
    pub(crate) fn create() -> Result<Self, ()> {
        let mut token = std::ptr::null_mut();
        // SAFETY: `token` is a live, aligned local out-parameter; on a zero status the
        // callee has written one newly owned handle that `Self` takes sole ownership of.
        (unsafe { hl_native_interrupt_create(&raw mut token) } == 0)
            .then(|| {
                NonNull::new(token).map(|handle| Self {
                    handle,
                    interrupted: std::sync::atomic::AtomicBool::new(false),
                })
            })
            .flatten()
            .ok_or(())
    }

    #[allow(dead_code)]
    pub(crate) fn set(&self, value: bool) -> Result<(), ()> {
        // SAFETY: `&self` keeps the handle alive, and the native side stores the flag
        // atomically — the documented way to interrupt a run from another thread.
        let result = (unsafe { hl_native_interrupt_set(self.handle.as_ptr(), u64::from(value)) } == 0)
            .then_some(())
            .ok_or(());
        if result.is_ok() {
            self.interrupted.store(value, std::sync::atomic::Ordering::Release);
        }
        result
    }

    pub(crate) fn is_set(&self) -> bool {
        self.interrupted.load(std::sync::atomic::Ordering::Acquire)
    }

    fn as_raw(&self) -> u64 {
        self.handle.as_ptr() as usize as u64
    }
}

impl hl_task::InterruptSink for InterruptToken {
    fn set_interrupted(&self, interrupted: bool) {
        if self.set(interrupted).is_err() {
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "native interrupt flag not published value={interrupted}"
            );
        }
    }
}

impl Drop for InterruptToken {
    fn drop(&mut self) {
        // SAFETY: Drop runs once with unique access, and `handle` has been solely owned
        // since `create`, so this destroys a live handle exactly once.
        unsafe { hl_native_interrupt_destroy(self.handle.as_ptr()) }
    }
}

#[repr(C)]
struct Mapping {
    abi: u32,
    size: u32,
    handle: u64,
    writable: u64,
    executable: u64,
    capacity: u64,
    content: u64,
}

#[repr(C)]
struct MemoryServices {
    abi: u32,
    size: u32,
    context: *mut c_void,
    reserve: unsafe extern "C" fn(*mut c_void, u64, u64, u32, *mut Mapping) -> u32,
    release: unsafe extern "C" fn(*mut c_void, u64) -> u32,
    publish: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> u32,
    repair: unsafe extern "C" fn(*mut c_void, *mut Mapping, u32) -> u32,
    write_begin: unsafe extern "C" fn(*mut c_void) -> u32,
    write_end: unsafe extern "C" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct Config {
    abi: u32,
    size: u32,
    capacity: u64,
    alignment: u64,
    flags: u32,
    reserved: u32,
    memory: *const MemoryServices,
}

#[repr(C)]
struct Change {
    abi: u32,
    size: u32,
    kind: u32,
    reserved: u32,
    first: u64,
    last: u64,
    mapping_epoch: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Diagnostics {
    abi: u32,
    size: u32,
    capacity: u64,
    used: u64,
    publications: u64,
    write_transitions: u64,
    dual_alias: u32,
    writing: u32,
    cache_lookups: u64,
    cache_hits: u64,
    cache_misses: u64,
    epoch_rejections: u64,
    invalidations: u64,
    live_blocks: u64,
    cache_generation: u64,
    mapping_epoch: u64,
    ibtc_fills: u64,
    ibtc_site_collisions: u64,
    ibtc_shared_collisions: u64,
    boundary_branch: u64,
    boundary_syscall: u64,
    boundary_fallback: u64,
    boundary_yield: u64,
    completed: u64,
    operand_callbacks: u64,
    operand_cache_hits: u64,
    x86_public_exits: u64,
    x86_public_syscalls: u64,
    x86_syscall_vector_dirty: u64,
    a64_guard_fast: u64,
    a64_guard_full: u64,
    a64_guard_fallback: u64,
    a64_dirty_reserved: u64,
    a64_dirty_overflow: u64,
    a64_dirty_committed: u64,
    a64_dirty_merged: u64,
    x86_cold_builds: u64,
    x86_cold_quota_exits: u64,
    relocation_cold_targets: u64,
    relocation_cycles: u64,
    relocation_capacity: u64,
    relocation_invalidations: u64,
    ibtc_site_misses: u64,
    ibtc_shared_misses: u64,
    a64_fallback_guard_read: u64,
    a64_fallback_guard_write: u64,
    a64_fallback_simd_fp: u64,
    a64_fallback_memory: u64,
    a64_fallback_control: u64,
    a64_fallback_other: u64,
    a64_fallback_entry_rejection: u64,
    a64_fallback_generated: u64,
    a64_fallback_call: u64,
    a64_fallback_return: u64,
    a64_fallback_indirect: u64,
    a64_fallback_system: u64,
    a64_fallback_form_memory: u64,
    a64_fallback_form_other: u64,
    x86_public_epochs: u64,
    a64_branch_exhaustion: u64,
    a64_branch_cold_relocation: u64,
    a64_branch_nonrelocatable: u64,
    a64_branch_unidentified: u64,
    a64_branch_sample_pc: u64,
    a64_branch_sample_source_first: u64,
    a64_branch_sample_source_last: u64,
    a64_branch_sample_form: u64,
    ibtc_authenticated_entries: u64,
    ibtc_shared_hits: u64,
    ibtc_auth_rejections: u64,
}

#[cfg(test)]
const A64_BRANCH_FORM_EXHAUSTION: u64 = 1;

#[derive(Clone, Copy)]
pub(crate) struct BorrowedSource<'a> {
    pub(crate) guest_first: u64,
    pub(crate) bytes: &'a [u8],
}

impl BorrowedSource<'_> {
    fn instruction_at(&self, pc: u64) -> Option<InstructionWord> {
        let offset = usize::try_from(pc.checked_sub(self.guest_first)?).ok()?;
        InstructionWord::read(self.bytes.get(offset..offset.checked_add(4)?)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstructionWord(u32);

impl InstructionWord {
    pub(crate) fn read(bytes: &[u8]) -> Option<Self> {
        Some(Self(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)))
    }

    pub(crate) fn literal_interval(self, pc: u64) -> Option<(u64, u64)> {
        let top = self.0 & 0xff00_0000;
        let width = match top {
            0x1800_0000 | 0x9800_0000 => 4_u64,
            0x5800_0000 => 8_u64,
            _ => return None,
        };
        let immediate = i64::from(((self.0 >> 5) & 0x7ffff) as i32);
        let displacement = (immediate << 45) >> 43;
        let target = pc.checked_add_signed(displacement)?;
        Some((target, target.checked_add(width)?))
    }

    /// A SIMD/FP load or store: top-level "loads and stores" class with the
    /// vector bit set. `scalar_access` declines these, so the direct authority
    /// has no operand it can certify for them.
    fn vector_access(self) -> bool {
        self.0 & 0x0a00_0000 == 0x0800_0000 && self.0 & 0x0400_0000 != 0
    }

    fn scalar_access(self) -> Option<Protection> {
        let word = self.0;
        if word & 0x0400_0000 != 0 || (word >> 30) & 3 == 3 && (word >> 22) & 3 == 2 {
            return None;
        }
        let eligible = word & 0x3b00_0000 == 0x3900_0000
            || word & 0x3b20_0000 == 0x3800_0000 && (word >> 10) & 3 == 0
            || word & 0x3b20_0c00 == 0x3820_0800 && matches!((word >> 13) & 7, 2 | 3 | 6 | 7);
        eligible.then_some(if (word >> 22) & 3 == 0 {
            Protection::WRITE
        } else {
            Protection::READ
        })
    }
}

fn direct_literal_target(pc: u64, sources: &[BorrowedSource<'_>], first: u64, last: u64) -> bool {
    let Some((target, end)) = sources
        .iter()
        .find_map(|source| source.instruction_at(pc))
        .and_then(|instruction| instruction.literal_interval(pc))
    else {
        return false;
    };
    target >= first && end <= last
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunStatistics {
    pub(crate) builds: u64,
    pub(crate) hits: u64,
    pub(crate) fallback: bool,
    /// A guard fault under direct authority, which carries no operand resolver: a limit
    /// of the run mode rather than a verdict on the entry.
    pub(crate) direct_guard: bool,
    /// Whether the run took direct authority. The cache identity carries the run mode, so
    /// alternating it between entries reissues the identity and resets every translation.
    pub(crate) direct: bool,
    pub(crate) sources: Vec<(u64, u64, ExecutableToken)>,
    pub(crate) sources_complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunOutcome {
    pub(crate) exit: Exit,
    pub(crate) instruction: u64,
    pub(crate) code: u64,
    pub(crate) remaining: u64,
    pub(crate) executed: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct X86RunOutcome {
    pub(crate) exit: Exit,
    pub(crate) instruction: u64,
    pub(crate) next: u64,
    pub(crate) address: u64,
    pub(crate) code: u64,
    pub(crate) remaining: u64,
    pub(crate) executed: u64,
}

struct ExecutableMemory {
    writable: *mut c_void,
    executable: *mut c_void,
    capacity: usize,
    descriptor: i32,
    releases: u64,
    write_transitions: u64,
    published_ranges: u64,
    published_bytes: u64,
}

impl ExecutableMemory {
    const fn new() -> Self {
        Self {
            writable: std::ptr::null_mut(),
            executable: std::ptr::null_mut(),
            capacity: 0,
            descriptor: -1,
            releases: 0,
            write_transitions: 0,
            published_ranges: 0,
            published_bytes: 0,
        }
    }

    fn protect(&self, protection: i32) -> u32 {
        if self.writable.is_null() || self.capacity == 0 {
            return 4;
        }
        // SAFETY: writable/capacity describe the unique live mapping owned here.
        if unsafe { libc::mprotect(self.writable, self.capacity, protection) } == 0 {
            0
        } else {
            3
        }
    }

    fn dual(&self) -> bool {
        self.writable != self.executable
    }

    #[cfg(target_os = "linux")]
    unsafe fn allocate_dual(capacity: usize) -> Result<(*mut c_void, *mut c_void, i32), u32> {
        // SAFETY: the name is a `'static` NUL-terminated literal; memfd_create takes no
        // output buffer and returns a newly owned descriptor or a negative errno.
        let descriptor = unsafe { libc::memfd_create(c"hl-native-code".as_ptr(), libc::MFD_CLOEXEC) };
        if descriptor < 0 {
            return Err(2);
        }
        // SAFETY: `descriptor` is the memfd just created and solely owned here; ftruncate
        // passes only integers and cannot alias caller memory.
        if unsafe { libc::ftruncate(descriptor, capacity as libc::off_t) } != 0 {
            // SAFETY: nothing has been mapped from `descriptor` yet, so this closes a
            // solely-owned fd exactly once on the error path.
            let _ = unsafe { libc::close(descriptor) };
            return Err(2);
        }
        // SAFETY: a null hint lets the kernel choose a fresh address, so this creates a
        // brand-new mapping that aliases nothing; `descriptor` was sized by ftruncate.
        let writable = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                capacity,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                descriptor,
                0,
            )
        };
        if writable == libc::MAP_FAILED {
            // SAFETY: no mapping survives on this path, so the solely-owned fd is closed
            // exactly once.
            let _ = unsafe { libc::close(descriptor) };
            return Err(2);
        }
        // SAFETY: null hint again, so the executable alias of the same memfd lands at a
        // fresh address; W^X is kept by giving this view PROT_EXEC and never PROT_WRITE.
        let executable = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                capacity,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_SHARED,
                descriptor,
                0,
            )
        };
        if executable == libc::MAP_FAILED {
            // SAFETY: `writable` is the mapping this function just created and has not
            // escaped, so it is unmapped exactly once with its own length.
            let _ = unsafe { libc::munmap(writable, capacity) };
            // SAFETY: the last mapping of `descriptor` was just released above, so the
            // solely-owned fd is closed exactly once.
            let _ = unsafe { libc::close(descriptor) };
            return Err(2);
        }
        Ok((writable, executable, descriptor))
    }
}

impl Drop for ExecutableMemory {
    fn drop(&mut self) {
        if !self.writable.is_null() {
            // SAFETY: this owner releases each distinct alias at most once.
            let _ = unsafe { libc::munmap(self.writable, self.capacity) };
            if self.dual() {
                let _ = unsafe { libc::munmap(self.executable, self.capacity) };
            }
            if self.descriptor >= 0 {
                // SAFETY: both aliases were unmapped just above and Drop has unique access,
                // so the memfd this owner allocated is closed exactly once.
                let _ = unsafe { libc::close(self.descriptor) };
            }
            self.writable = std::ptr::null_mut();
            self.executable = std::ptr::null_mut();
        }
    }
}

unsafe extern "C" fn reserve(
    context: *mut c_void,
    capacity: u64,
    _alignment: u64,
    dual: u32,
    output: *mut Mapping,
) -> u32 {
    if context.is_null() || output.is_null() || dual > 1 || capacity > usize::MAX as u64 {
        return 1;
    }
    // SAFETY: C passes back the context supplied by create and a writable output.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    if !memory.writable.is_null() {
        return 4;
    }
    #[cfg(target_os = "linux")]
    let (writable, executable, descriptor) = if dual != 0 {
        // SAFETY: `memory.writable` was checked null above, so no mapping is live to leak,
        // and `capacity` was range-checked against usize::MAX at entry.
        match unsafe { ExecutableMemory::allocate_dual(capacity as usize) } {
            Ok(mapping) => mapping,
            Err(status) => return status,
        }
    } else {
        // SAFETY: null hint with an anonymous private mapping creates fresh memory that
        // aliases nothing; `capacity` was range-checked at entry.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                capacity as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return 2;
        }
        (address, address, -1)
    };
    #[cfg(not(target_os = "linux"))]
    let (writable, executable, descriptor) = {
        if dual != 0 {
            return 2;
        }
        // SAFETY: null hint with an anonymous private mapping creates fresh memory that
        // aliases nothing; `capacity` was range-checked at entry.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                capacity as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return 2;
        }
        (address, address, -1)
    };
    memory.writable = writable;
    memory.executable = executable;
    memory.descriptor = descriptor;
    memory.capacity = capacity as usize;
    // SAFETY: output was validated and is uniquely owned for this callback.
    unsafe {
        *output = Mapping {
            abi: ABI,
            size: std::mem::size_of::<Mapping>() as u32,
            handle: 1,
            writable: writable as usize as u64,
            executable: executable as usize as u64,
            capacity,
            content: 0,
        };
    }
    0
}

unsafe extern "C" fn release(context: *mut c_void, handle: u64) -> u32 {
    if context.is_null() || handle != 1 {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the `Box<ExecutableMemory>` the
    // Executor owns and outlives every callback; the engine never reenters these
    // callbacks concurrently, so this `&mut` is the only live reference.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    if memory.writable.is_null() {
        return 4;
    }
    let writable = memory.writable;
    let executable = memory.executable;
    let capacity = memory.capacity;
    let descriptor = memory.descriptor;
    memory.writable = std::ptr::null_mut();
    memory.executable = std::ptr::null_mut();
    memory.capacity = 0;
    memory.descriptor = -1;
    memory.releases += 1;
    // SAFETY: the fields were cleared just above, so no later call can see these
    // pointers; this unmaps the owned mapping once with the length it was created with.
    let writable_status = unsafe { libc::munmap(writable, capacity) };
    let executable_status = if writable == executable {
        0
    } else {
        // SAFETY: reached only when the executable alias is a distinct mapping, so this
        // releases the second owned alias exactly once.
        unsafe { libc::munmap(executable, capacity) }
    };
    if descriptor >= 0 {
        // SAFETY: both aliases are unmapped above and the field was cleared, so the
        // memfd is closed exactly once.
        let _ = unsafe { libc::close(descriptor) };
    }
    if writable_status != 0 || executable_status != 0 {
        return 3;
    }
    0
}

unsafe extern "C" fn publish(context: *mut c_void, handle: u64, offset: u64, size: u64) -> u32 {
    if context.is_null() || handle != 1 {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the Executor-owned
    // `Box<ExecutableMemory>`, which outlives every callback and is not reentered
    // concurrently, so this `&mut` is the only live reference.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    if offset > memory.capacity as u64 || size > memory.capacity as u64 - offset {
        return 1;
    }
    if size != 0 {
        // SAFETY: the check above proves offset + size <= capacity, so the offset stays
        // within the single live executable mapping and cannot wrap.
        let address = unsafe { memory.executable.cast::<u8>().add(offset as usize).cast() };
        // SAFETY: `address..address + size` lies inside that mapping, which stays live for
        // the call; the flush only maintains icache coherence and retains no pointer.
        unsafe { hl_native_flush(address, size as usize) };
        memory.published_ranges += 1;
        memory.published_bytes += size;
    }
    0
}

unsafe extern "C" fn repair(context: *mut c_void, mapping: *mut Mapping, preserve: u32) -> u32 {
    if context.is_null() || mapping.is_null() {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the Executor-owned
    // `Box<ExecutableMemory>`, live for the whole callback and not reentered.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    // SAFETY: `mapping` was null-checked and points to the engine's Mapping record, which
    // it lends exclusively for this call; it is disjoint from `memory`.
    let current = unsafe { &mut *mapping };
    if current.handle != 1
        || current.writable != memory.writable as usize as u64
        || current.executable != memory.executable as usize as u64
    {
        return 1;
    }
    if !memory.dual() {
        if preserve != 0 {
            return memory.protect(libc::PROT_READ | libc::PROT_EXEC);
        }
        // SAFETY: null hint, so this fresh anonymous mapping aliases nothing; the old
        // mapping is only unmapped after both `memory` and `current` point at this one.
        let replacement = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                memory.capacity,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if replacement == libc::MAP_FAILED {
            return 2;
        }
        let old = memory.writable;
        memory.writable = replacement;
        memory.executable = replacement;
        current.writable = replacement as usize as u64;
        current.executable = replacement as usize as u64;
        current.content = 0;
        // SAFETY: `old` was the sole (non-dual) mapping and both `memory` and `current`
        // now name the replacement, so nothing can observe it after this single unmap.
        return if unsafe { libc::munmap(old, memory.capacity) } == 0 {
            0
        } else {
            3
        };
    }
    #[cfg(target_os = "linux")]
    {
        let old_writable = memory.writable;
        let old_executable = memory.executable;
        let old_descriptor = memory.descriptor;
        let capacity = memory.capacity;
        // SAFETY: the old pointers are saved in locals above, so the fresh pair allocated
        // here leaks nothing; every early return below unmaps whatever it allocated.
        let (mut writable, mut executable, descriptor) = match unsafe { ExecutableMemory::allocate_dual(capacity) } {
            Ok(replacement) => replacement,
            Err(status) => return status,
        };
        if preserve != 0 {
            if current.content > capacity as u64 {
                // SAFETY: these three are the freshly allocated pair and its memfd, not yet
                // stored anywhere, so each is released exactly once on this reject path.
                let _ = unsafe { libc::munmap(executable, capacity) };
                // SAFETY: same freshly allocated writable alias, released once.
                let _ = unsafe { libc::munmap(writable, capacity) };
                // SAFETY: both new aliases are gone, so the new memfd is closed once.
                let _ = unsafe { libc::close(descriptor) };
                return 1;
            }
            // SAFETY: `content <= capacity` was just checked, and the old and new writable
            // mappings are distinct kernel-chosen regions, so the ranges cannot overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(old_writable.cast::<u8>(), writable.cast(), current.content as usize);
            }
            // SAFETY: the new executable alias is dropped here so the memfd can instead be
            // remapped at the old address below; `writable` still holds the copied bytes.
            let _ = unsafe { libc::munmap(executable, capacity) };
            // SAFETY: MAP_FIXED over `old_executable` atomically replaces the stale alias
            // in place, so code addresses the engine already handed out stay valid.
            executable = unsafe {
                libc::mmap(
                    old_executable,
                    capacity,
                    libc::PROT_READ | libc::PROT_EXEC,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    descriptor,
                    0,
                )
            };
            if executable == libc::MAP_FAILED {
                // SAFETY: only the new writable alias remains allocated on this path, so it
                // is unmapped once with its own length.
                let _ = unsafe { libc::munmap(writable, capacity) };
                // SAFETY: no mapping of the new memfd survives, so it is closed once.
                let _ = unsafe { libc::close(descriptor) };
                return 2;
            }
            // SAFETY: MAP_FIXED over `old_writable` atomically replaces the stale writable
            // alias in place, keeping the address the engine already recorded.
            let fixed_writable = unsafe {
                libc::mmap(
                    old_writable,
                    capacity,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    descriptor,
                    0,
                )
            };
            if fixed_writable == libc::MAP_FAILED {
                // SAFETY: the executable alias installed above is still reachable via the
                // memfd, so only the descriptor reference is dropped here, exactly once.
                let _ = unsafe { libc::close(descriptor) };
                return 2;
            }
            // SAFETY: the temporary kernel-chosen writable alias has been superseded by
            // `fixed_writable`, so this releases that now-redundant mapping once.
            let _ = unsafe { libc::munmap(writable, capacity) };
            writable = fixed_writable;
        } else {
            // SAFETY: discard mode keeps no contents, so the two old aliases are each
            // unmapped once; the new pair already replaces them in the locals above.
            let _ = unsafe { libc::munmap(old_executable, capacity) };
            // SAFETY: the second old alias, unmapped exactly once with its own length.
            let _ = unsafe { libc::munmap(old_writable, capacity) };
        }
        if old_descriptor >= 0 {
            // SAFETY: every mapping of the old memfd has been replaced or unmapped above,
            // so the old descriptor is closed exactly once.
            let _ = unsafe { libc::close(old_descriptor) };
        }
        memory.writable = writable;
        memory.executable = executable;
        memory.descriptor = descriptor;
        current.writable = writable as usize as u64;
        current.executable = executable as usize as u64;
        if preserve == 0 {
            current.content = 0;
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    0
}

unsafe extern "C" fn write_begin(context: *mut c_void) -> u32 {
    if context.is_null() {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the Executor-owned
    // `Box<ExecutableMemory>`; the engine brackets write_begin/write_end without
    // reentering, so this `&mut` is the only live reference.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    let status = if memory.dual() {
        0
    } else {
        memory.protect(libc::PROT_READ | libc::PROT_WRITE)
    };
    if status == 0 {
        memory.write_transitions += u64::from(!memory.dual());
    }
    status
}

unsafe extern "C" fn write_end(context: *mut c_void) -> u32 {
    if context.is_null() {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the Executor-owned
    // `Box<ExecutableMemory>`; this closes the write window opened by write_begin, so
    // the `&mut` is again the only live reference.
    let memory = unsafe { &mut *context.cast::<ExecutableMemory>() };
    let status = if memory.dual() {
        0
    } else {
        memory.protect(libc::PROT_READ | libc::PROT_EXEC)
    };
    if status == 0 {
        memory.write_transitions += u64::from(!memory.dual());
    }
    status
}

#[repr(C)]
pub(crate) struct SourceSpan {
    pub(crate) guest_first: u64,
    pub(crate) bytes: *const u8,
    pub(crate) size: usize,
    pub(crate) mapping_incarnation: u64,
    pub(crate) instruction_epoch: u64,
}

#[repr(C)]
pub(crate) struct Source {
    pub(crate) spans: *const SourceSpan,
    pub(crate) span_count: usize,
    pub(crate) mapping_incarnation: u64,
    pub(crate) instruction_epoch: u64,
}

struct SourceProvider<'a> {
    resolve: &'a mut dyn FnMut(u64, &mut [u8]) -> Option<(usize, ExecutableToken)>,
    bytes: [u8; 256],
    observed: Vec<(u64, u64, ExecutableToken)>,
    complete: bool,
    #[cfg(test)]
    boundary_capture: Option<&'a std::sync::Mutex<BoundaryCaptureState>>,
}

type SourceResolve = unsafe extern "C" fn(*mut c_void, u64, u64, u64, *mut SourceSpan) -> i32;
type OperandResolve = unsafe extern "C" fn(*mut c_void, u64, u64, u32, u64, u64, *mut ProjectionView) -> u32;

struct OperandProvider<'lease, 'memory, H: MemoryAccessHost> {
    lease: &'lease mut ProjectionLease<'memory, H>,
    observed: &'lease mut ViewHints,
    #[cfg(test)]
    boundary_capture: Option<&'lease std::sync::Mutex<BoundaryCaptureState>>,
}

#[derive(Default)]
struct ViewHints {
    entries: Vec<(u64, Protection)>,
    epoch: Option<[u64; 4]>,
}

impl ViewHints {
    const LIMIT: usize = 3;

    fn begin(&mut self, epoch: [u64; 4]) {
        if self.epoch != Some(epoch) {
            self.entries.clear();
            self.epoch = Some(epoch);
        }
    }

    /// One slot per address holding the union of the accesses seen there. Keying on
    /// `(address, protection)` let a read and a write of the same address take two of
    /// the three slots and publish two views whose permissions each reject the other
    /// access, so every alternation missed the guard.
    fn observe(&mut self, address: u64, required: Protection) {
        let widened = self
            .entries
            .iter()
            .find(|existing| existing.0 == address)
            .map_or(required, |existing| existing.1.union(required));
        self.entries.retain(|existing| existing.0 != address);
        self.entries.insert(0, (address, widened));
        self.entries.truncate(Self::LIMIT);
    }
}

unsafe extern "C" fn resolve_operand<H: MemoryAccessHost>(
    context: *mut c_void,
    address: u64,
    size: u64,
    access: u32,
    mapping_incarnation: u64,
    _instruction_epoch: u64,
    output: *mut ProjectionView,
) -> u32 {
    if context.is_null() || output.is_null() {
        return 0;
    }
    // SAFETY: `context` is the null-checked pointer to the `OperandProvider` the caller
    // stack-pins for the duration of the run; generated code calls this synchronously on
    // the running thread, so this `&mut` is the only live reference.
    let provider = unsafe { &mut *context.cast::<OperandProvider<'_, '_, H>>() };
    let generation = provider.lease.generation();
    if generation.incarnation != mapping_incarnation {
        return 3;
    }
    let Some(required) = Protection::from_bits(u8::try_from(access).ok().unwrap_or(u8::MAX)) else {
        return 0;
    };
    /// `project_bounded` clamps to the resolved region, so an unbounded span
    /// projects the whole containing mapping in one view.
    const OPERAND_SPAN: u64 = u64::MAX;
    // The four run views are keyed by permission as well as range, so a read-only view of a
    // writable region costs a second slot and thrashes the cache; widen reads to read/write.
    let widened = required.union(Protection::READ).union(Protection::WRITE);
    let projection = provider
        .lease
        .project_bounded(hl_isa::GuestAddress::new(address), size, widened, OPERAND_SPAN)
        .or_else(|_| {
            provider
                .lease
                .project_bounded(hl_isa::GuestAddress::new(address), size, required, OPERAND_SPAN)
        });
    match projection {
        Ok(view) => {
            provider.observed.observe(address, required);
            let permissions = view_permissions(provider.lease, view.protection, view.range.start());
            let projected = ProjectionView {
                guest_first: view.range.start().get(),
                guest_last: view.range.end().get(),
                host_first: view.storage_address,
                mapping_incarnation,
                permissions,
                write_policy: WRITE_EXACT,
                write_index: view.index,
            };
            #[cfg(test)]
            if let Some(capture) = provider.boundary_capture {
                capture
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .append_view(&projected);
            }
            // SAFETY: `output` was null-checked and is the engine's writable out-parameter
            // for this call, aligned for ProjectionView and disjoint from `context`;
            // `write` needs no valid prior value.
            unsafe {
                output.write(projected);
            }
            1
        }
        Err(MemoryError::NoAddressSpace | MemoryError::AddressOverflow | MemoryError::EmptyRange) => 2,
        Err(_) => 0,
    }
}

unsafe extern "C" fn resolve_source(
    context: *mut c_void,
    guest_pc: u64,
    mapping_incarnation: u64,
    _instruction_epoch: u64,
    output: *mut SourceSpan,
) -> i32 {
    if context.is_null() || output.is_null() {
        return 0;
    }
    // SAFETY: run_aarch64 passes a live, uniquely borrowed SourceProvider for
    // the duration of the synchronous native call and a writable C output.
    let provider = unsafe { &mut *context.cast::<SourceProvider<'_>>() };
    let Some((size, token)) = (provider.resolve)(guest_pc, &mut provider.bytes) else {
        return 0;
    };
    if size == 0 || size > provider.bytes.len() {
        return 0;
    }
    if token.incarnation != mapping_incarnation || guest_pc > u64::MAX - size as u64 {
        provider.complete = false;
        return 0;
    }
    let observation = (guest_pc, guest_pc + size as u64, token);
    #[cfg(test)]
    if let Some(capture) = provider.boundary_capture {
        capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .append_source(guest_pc, &provider.bytes[..size], token);
    }
    if let Some(existing) = provider
        .observed
        .iter()
        .find(|(first, last, _)| *first == observation.0 && *last == observation.1)
    {
        if existing.2 != token {
            provider.complete = false;
            return 0;
        }
    } else if provider.observed.len() == 1024 {
        provider.complete = false;
        return 0;
    } else {
        provider.observed.push(observation);
    }
    // SAFETY: output was validated non-null and points to the C-owned result.
    unsafe {
        output.write(SourceSpan {
            guest_first: guest_pc,
            bytes: provider.bytes.as_ptr(),
            size,
            mapping_incarnation,
            instruction_epoch: token.version,
        });
    }
    1
}

#[repr(C)]
pub(crate) struct ProjectionView {
    pub(crate) guest_first: u64,
    pub(crate) guest_last: u64,
    pub(crate) host_first: u64,
    pub(crate) mapping_incarnation: u64,
    pub(crate) permissions: u32,
    pub(crate) write_policy: u16,
    pub(crate) write_index: u16,
}

#[repr(C)]
pub(crate) struct Projection {
    pub(crate) views: *const ProjectionView,
    pub(crate) count: usize,
    pub(crate) mapping_incarnation: u64,
    pub(crate) active: usize,
}

#[repr(C)]
struct RunRequest {
    abi: u32,
    size: u32,
    architecture: u32,
    reserved: u32,
    mapping_epoch: u64,
    budget: u64,
    source: *const Source,
    projection: *const Projection,
    source_context: *mut c_void,
    source_resolve: Option<SourceResolve>,
    operand_context: *mut c_void,
    operand_resolve: Option<OperandResolve>,
    fault_context: *mut c_void,
    fault_publish: Option<unsafe extern "C" fn(*mut c_void, *const FaultScope) -> u32>,
    fault_unpublish: Option<unsafe extern "C" fn(*mut c_void, *const FaultScope)>,
    memory_mode: u64,
    authority_generation: u64,
    direct_token: *const DirectToken,
    authority_identity: u64,
    quantum_context: *mut c_void,
    quantum_poll: Option<unsafe extern "C" fn(*mut c_void, u64, u64) -> u32>,
    quantum_grant: u64,
    certificate: *const RunCertificate,
}

#[repr(C)]
struct RunCertificate {
    abi: u32,
    size: u32,
    architecture: u32,
    data_permissions: u32,
    mapped_executable: u32,
    view_index: u32,
    write_policy: u16,
    reserved: u16,
    reserved2: u32,
    guest_first: u64,
    guest_last: u64,
    host_first: u64,
    mapping_incarnation: u64,
    mapping_generation: u64,
    instruction_generation: u64,
    authority_identity: u64,
    authority_generation: u64,
    run_generation: u64,
    direct_token: *const DirectToken,
}

unsafe extern "C" fn poll_quantum(context: *mut c_void, executed: u64, admitted: u64) -> u32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: run_x86_inner supplies a uniquely borrowed closure which remains
    // alive for the synchronous native run. Native invokes it serially only at
    // fully spilled REP boundaries. Catching panic prevents unwind across FFI.
    let poll = unsafe { &mut *context.cast::<&mut dyn FnMut(u64, u64) -> bool>() };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll(executed, admitted)))
        .ok()
        .is_some_and(|grant| grant) as u32
}

#[repr(C)]
struct CpuHandle {
    abi: u32,
    size: u32,
    architecture: u32,
    reserved: u32,
    state: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FaultScope {
    abi: u32,
    size: u32,
    architecture: u32,
    reserved: u32,
    executor: *mut Handle,
    cpu: *mut CpuHandle,
}

#[derive(Clone, Copy)]
pub(crate) struct HostFaultView(FaultScope);
unsafe impl Send for HostFaultView {}
unsafe impl Sync for HostFaultView {}

#[cfg(test)]
impl HostFaultView {
    /// # Safety
    /// The view must still be inside its matching publish/unpublish interval.
    unsafe fn contains(self, host_pc: u64) -> bool {
        // SAFETY: the caller's `# Safety` contract puts this inside the matching
        // publish/unpublish interval, so the scope's executor and cpu are still live;
        // the pointer is to a local copy and the query only reads.
        unsafe { hl_native_fault_scope_contains(&raw const self.0, host_pc) != 0 }
    }
}

/// Consumer contract for associating one borrowed native fault view with the
/// current host thread.
///
/// # Safety
/// `attach` and `detach` bracket the complete host scheduler-thread lifetime;
/// every publish must be retired before detach, and the process owner must
/// outlive both the attachment and every native executor using it.
///
/// A published view and every copy of it borrow executor/cache/CPU storage only
/// until the matching `unpublish`. Implementations must make the view visible
/// to at most the current thread's synchronous fault handler, must synchronously
/// retire every copy before `unpublish` returns, must never call it afterward,
/// and must not unwind from either callback.
pub(crate) unsafe trait HostFaultOwner: Send + Sync {
    fn attach(&self) -> Result<(), ()> {
        Ok(())
    }
    fn detach(&self) {}
    unsafe fn publish(&self, view: HostFaultView) -> Result<(), ()>;
    unsafe fn unpublish(&self, view: HostFaultView);
}

#[cfg(target_os = "linux")]
std::thread_local! {
    static FAULT_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Process owner for the native POSIX fault dispatcher. Thread association is
/// explicit because the alternate stack belongs to the scheduler host thread.
#[cfg(target_os = "linux")]
pub(crate) struct NativeFaultOwner;

#[cfg(target_os = "linux")]
impl NativeFaultOwner {
    pub(crate) fn create() -> Result<std::sync::Arc<dyn HostFaultOwner>, ()> {
        // SAFETY: takes no arguments and installs the process-wide fault dispatcher; the
        // matching release runs in this type's Drop, so acquire/release stay balanced.
        (unsafe { hl_native_fault_process_acquire() } == 0)
            .then(|| std::sync::Arc::new(Self) as std::sync::Arc<dyn HostFaultOwner>)
            .ok_or(())
    }
}

#[cfg(target_os = "linux")]
unsafe impl HostFaultOwner for NativeFaultOwner {
    fn attach(&self) -> Result<(), ()> {
        // SAFETY: argument-free, and the `NativeFaultOwner` receiver proves the process
        // dispatcher is still acquired; this installs the alternate stack for this thread.
        let attached = unsafe { hl_native_fault_thread_attach() } == 0;
        if attached {
            FAULT_GENERATION.with(|generation| generation.set(0));
            Ok(())
        } else {
            Err(())
        }
    }

    fn detach(&self) {
        let inactive = FAULT_GENERATION.with(|generation| generation.get() == 0);
        if inactive {
            // SAFETY: the zero thread-local generation proves no view is still published
            // on this thread, so tearing down its alternate stack strands no fault scope.
            let _ = unsafe { hl_native_fault_thread_detach() };
        }
    }

    unsafe fn publish(&self, view: HostFaultView) -> Result<(), ()> {
        let mut generation = 0;
        // SAFETY: the trait's contract guarantees the view's executor and cpu stay live
        // until the matching unpublish; both pointers are live locals for the call, and
        // the scope is only made visible to this thread's own fault handler.
        if unsafe { hl_native_fault_thread_publish(&raw const view.0, &raw mut generation) } != 0 {
            return Err(());
        }
        FAULT_GENERATION.with(|current| current.set(generation));
        Ok(())
    }

    unsafe fn unpublish(&self, view: HostFaultView) {
        let generation = FAULT_GENERATION.with(|current| current.replace(0));
        // The generation is restored below if the retire fails.
        if generation != 0
            // SAFETY: a nonzero thread-local generation proves a matching publish is
            // outstanding on this thread, so this retires that scope exactly once.
            && unsafe { hl_native_fault_thread_unpublish(&raw const view.0, generation) } != 0
        {
            FAULT_GENERATION.with(|current| current.set(generation));
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativeFaultOwner {
    fn drop(&mut self) {
        // SAFETY: Drop runs once and only after every Arc clone is gone, balancing the
        // single acquire in `create`; the trait contract has this owner outlive every
        // attachment and executor that used it.
        let _ = unsafe { hl_native_fault_process_release() };
    }
}

unsafe extern "C" fn fault_publish(context: *mut c_void, scope: *const FaultScope) -> u32 {
    if context.is_null() || scope.is_null() {
        return 1;
    }
    // SAFETY: `context` is the null-checked pointer to the `fault_owner` Arc field of the
    // Executor borrowed for this run, so it outlives the callback; the reference is shared
    // and `HostFaultOwner: Sync`.
    let owner = unsafe { &*context.cast::<std::sync::Arc<dyn HostFaultOwner>>() };
    // SAFETY: the engine calls this on the running thread while the scope it points at is
    // live, so `*scope` copies an initialized record and the publish contract is met;
    // catch_unwind keeps a panic from unwinding across the C frame.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        owner.publish(HostFaultView(*scope))
    }))
    .map_or(4, |result| result.map_or(4, |()| 0))
}

unsafe extern "C" fn fault_unpublish(context: *mut c_void, scope: *const FaultScope) {
    if context.is_null() || scope.is_null() {
        return;
    }
    // SAFETY: `context` is the null-checked pointer to the same Executor-owned
    // `fault_owner` Arc, still borrowed for this run; the reference is shared and
    // `HostFaultOwner: Sync`.
    let owner = unsafe { &*context.cast::<std::sync::Arc<dyn HostFaultOwner>>() };
    // catch_unwind keeps a panic from unwinding across the C frame.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: paired with the `fault_publish` above on the same thread, so `*scope`
        // reads the same live record and retires exactly that publish.
        unsafe { owner.unpublish(HostFaultView(*scope)) };
    }));
}

#[repr(C)]
struct RunExit {
    abi: u32,
    size: u32,
    kind: u32,
    access: u32,
    instruction: u64,
    next: u64,
    address: u64,
    code: u64,
}

struct NativeAarch64(schema::Aarch64Cpu);

impl NativeAarch64 {
    fn capture(cpu: &Aarch64CpuState) -> Self {
        let mut vectors = [0; 64];
        for (lanes, value) in vectors.chunks_exact_mut(2).zip(cpu.vectors) {
            lanes[0] = value as u64;
            lanes[1] = (value >> 64) as u64;
        }
        Self(schema::Aarch64Cpu {
            registers: cpu.registers,
            stack: cpu.sp,
            program: cpu.pc,
            tls: cpu.tls,
            reason: 0,
            host_stack: 0,
            host_registers: [0; 12],
            vectors,
            host_vectors: [0; 16],
            flags: u64::from(cpu.nzcv.bits()),
            indirect_site: 0,
            interrupt: 0,
            memory_first: 0,
            memory_last: 0,
            memory_delta: 0,
            memory_permissions: 0,
            fault_address: 0,
            fault_access: 0,
            fault_size: 0,
            budget: 0,
            executed: 0,
            memory_written: 0,
            dirty_view_first: 0,
            dirty_view_last: 0,
            dirty_first: u64::MAX,
            dirty_last: 0,
            dirty_count: 0,
            dirty_overflow: 0,
            dirty_records: [[0; 4]; 16],
            read_token: 0,
            read_incarnation: 0,
            read_count: 0,
            read_views: [[0; 4]; 4],
            interrupt_token: 0,
            executable_written: 0,
            fpcr: cpu.fpcr,
            fpsr: cpu.fpsr,
            active_authority: 0,
            loop_valid: 0,
            loop_view_count: 0,
            loop_views: [[0; 6]; 2],
            loop_mapping_incarnation: 0,
            loop_authority: 0,
            loop_trip: 0,
            loop_decrement: 0,
            loop_instruction_count: 0,
            loop_iterations: 0,
            loop_budget_iterations: 0,
            loop_executable: 0,
            active_view_incarnation: 0,
            active_view_authority: 0,
            diagnostic_guard_fast: 0,
            diagnostic_guard_full: 0,
            diagnostic_guard_fallback: 0,
            diagnostic_dirty_reserved: 0,
            diagnostic_dirty_overflow: 0,
            diagnostic_dirty_committed: 0,
            diagnostic_dirty_merged: 0,
            read_view_publication: [[0; 2]; 4],
            memory_write_policy: 0,
            memory_write_index: 0,
            certificate_guest_first: 0,
            certificate_guest_last: 0,
            certificate_host_first: 0,
            certificate_data_permissions: 0,
            certificate_mapped_executable: 0,
            certificate_mapping_incarnation: 0,
            certificate_mapping_generation: 0,
            certificate_instruction_generation: 0,
            certificate_authority_identity: 0,
            certificate_authority_generation: 0,
            certificate_run_generation: 0,
            certificate_view_index: 0,
            certificate_write_policy: 0,
            certificate_cache_identity: 0,
            certificate_token: 0,
            diagnostic_ibtc_authenticated_entries: 0,
            diagnostic_ibtc_shared_hits: 0,
            diagnostic_ibtc_auth_rejections: 0,
            code_arena_lower: 0,
            code_arena_upper: 0,
            entry_certificate_identity: 0,
            fault_completed: 0,
            ibtc_base: 0,
            execution_identity: 0,
            read_valid_count: 0,
        })
    }

    fn restore(&self, cpu: &mut Aarch64CpuState) {
        cpu.registers = self.0.registers;
        cpu.sp = self.0.stack;
        cpu.pc = self.0.program;
        cpu.tls = self.0.tls;
        cpu.nzcv = Nzcv::from_bits(self.0.flags as u32);
        cpu.fpcr = self.0.fpcr;
        cpu.fpsr = self.0.fpsr;
        for (value, lanes) in cpu.vectors.iter_mut().zip(self.0.vectors.chunks_exact(2)) {
            *value = u128::from(lanes[0]) | u128::from(lanes[1]) << 64;
        }
        cpu.clear_exclusive_reservation();
    }

    fn writes(&self) -> NativeWrites {
        let cpu = &self.0;
        if cpu.memory_written == 0 {
            return NativeWrites::None;
        }
        if cpu.dirty_overflow != 0 || cpu.dirty_count > cpu.dirty_records.len() as u64 {
            return NativeWrites::Full;
        }
        let mut records = cpu.dirty_records[..cpu.dirty_count as usize].to_vec();
        if cpu.dirty_first != u64::MAX {
            records.push([
                cpu.dirty_view_first,
                cpu.dirty_view_last,
                cpu.dirty_first,
                cpu.dirty_last,
            ]);
        }
        let mut ranges = Vec::with_capacity(records.len());
        for [view_first, view_last, first, last] in records {
            if view_first >= view_last || first < view_first || first >= last || last > view_last {
                return NativeWrites::Full;
            }
            let Ok(range) = AddressRange::nonempty(GuestAddress::new(first), last - first) else {
                return NativeWrites::Full;
            };
            ranges.push(range);
        }
        if ranges.is_empty() {
            NativeWrites::Full
        } else {
            NativeWrites::Exact(ranges)
        }
    }
}

struct NativeX86(schema::X86_64Cpu);

pub(crate) enum NativeWrites {
    None,
    Exact(Vec<AddressRange>),
    Full,
}

impl NativeX86 {
    fn writes(&self) -> NativeWrites {
        let cpu = &self.0;
        if cpu.memory_written == 0 {
            return NativeWrites::None;
        }
        if cpu.dirty_overflow != 0 || cpu.dirty_count > cpu.dirty_records.len() as u64 {
            return NativeWrites::Full;
        }
        let mut records = cpu.dirty_records[..cpu.dirty_count as usize].to_vec();
        if cpu.dirty_first != u64::MAX {
            records.push([
                cpu.dirty_view_first,
                cpu.dirty_view_last,
                cpu.dirty_first,
                cpu.dirty_last,
            ]);
        }
        let mut ranges = Vec::with_capacity(records.len());
        for [view_first, view_last, first, last] in records {
            if view_first >= view_last || first < view_first || first >= last || last > view_last {
                return NativeWrites::Full;
            }
            let Ok(range) = AddressRange::nonempty(GuestAddress::new(first), last - first) else {
                return NativeWrites::Full;
            };
            ranges.push(range);
        }
        if ranges.is_empty() {
            NativeWrites::Full
        } else {
            NativeWrites::Exact(ranges)
        }
    }

    fn fpcr(mxcsr: u32) -> u64 {
        // Retained-C projection: ARM RMode swaps the x86 down/up encodings and
        // ARM FZ represents either x86 DAZ or FTZ. Exception masks remain in
        // architectural MXCSR; AArch64 trap-enable bits are not equivalent to
        // a guest #XM and must stay clear until the native fault path owns it.
        let rounding = match mxcsr >> 13 & 3 {
            0 => 0,
            1 => 2,
            2 => 1,
            _ => 3,
        };
        let mut fpcr = rounding << 22;
        if mxcsr & ((1 << 15) | (1 << 6)) != 0 {
            fpcr |= 1 << 24;
        }
        fpcr
    }

    fn fpsr(mxcsr: u32) -> u64 {
        u64::from(mxcsr & 1)
            | u64::from(mxcsr >> 2 & 1) << 1
            | u64::from(mxcsr >> 3 & 1) << 2
            | u64::from(mxcsr >> 4 & 1) << 3
            | u64::from(mxcsr >> 5 & 1) << 4
            | u64::from(mxcsr >> 1 & 1) << 7
    }

    fn exception_flags(fpsr: u64) -> u32 {
        (fpsr as u32 & 1)
            | ((fpsr as u32 >> 7 & 1) << 1)
            | ((fpsr as u32 >> 1 & 1) << 2)
            | ((fpsr as u32 >> 2 & 1) << 3)
            | ((fpsr as u32 >> 3 & 1) << 4)
            | ((fpsr as u32 >> 4 & 1) << 5)
    }

    fn capture(cpu: &X86CpuState, interrupt: bool) -> Self {
        let mut vectors = [0; 32];
        for (lanes, value) in vectors.chunks_exact_mut(2).zip(cpu.vectors) {
            lanes[0] = value as u64;
            lanes[1] = (value >> 64) as u64;
        }
        let mut vector_upper = [0; 32];
        for (lanes, value) in vector_upper.chunks_exact_mut(2).zip(cpu.vector_upper) {
            lanes[0] = value as u64;
            lanes[1] = (value >> 64) as u64;
        }
        let flags = u64::from(cpu.flags.bits())
            | (u64::from(cpu.direction) << 10)
            | (u64::from(cpu.alignment_check) << 18)
            | (u64::from(cpu.id_flag) << 21);
        Self(schema::X86_64Cpu {
            registers: cpu.registers,
            program: cpu.rip,
            flags,
            fs: cpu.fs_base,
            gs: cpu.gs_base,
            reason: 0,
            host_stack: 0,
            host_registers: [0; 12],
            host_vectors: [0; 16],
            vectors,
            vector_upper,
            scratch: [0; 2],
            interrupt: u64::from(interrupt),
            indirect_site: 0,
            memory_first: 0,
            memory_last: 0,
            memory_delta: 0,
            memory_permissions: 0,
            fault_address: 0,
            fault_access: 0,
            fault_size: 0,
            memory_written: 0,
            budget: 0,
            executed: 0,
            loop_remaining: 0,
            loop_completed: 0,
            loop_block_count: 0,
            loop_pc: 0,
            dirty_view_first: 0,
            dirty_view_last: 0,
            dirty_first: u64::MAX,
            dirty_last: 0,
            dirty_count: 0,
            dirty_overflow: 0,
            dirty_records: [[0; 4]; 16],
            read_token: 0,
            read_incarnation: 0,
            read_count: 0,
            read_views: [[0; 4]; 4],
            executable_written: 0,
            mxcsr: u64::from(cpu.mxcsr),
            fpcr: Self::fpcr(cpu.mxcsr),
            fpsr: Self::fpsr(cpu.mxcsr),
            host_fpcr: 0,
            host_fpsr: 0,
            vector_dirty: 0,
            certificate_guest_first: 0,
            certificate_guest_last: 0,
            certificate_host_first: 0,
            certificate_data_permissions: 0,
            certificate_mapped_executable: 0,
            certificate_mapping_incarnation: 0,
            certificate_mapping_generation: 0,
            certificate_instruction_generation: 0,
            certificate_authority_identity: 0,
            certificate_authority_generation: 0,
            certificate_run_generation: 0,
            certificate_view_index: 0,
            certificate_write_policy: 0,
            certificate_cache_identity: 0,
            certificate_token: 0,
        })
    }

    fn restore(&self, cpu: &mut X86CpuState) {
        cpu.registers = self.0.registers;
        cpu.rip = self.0.program;
        cpu.flags = FlagState::from_bits((self.0.flags as u16) & !(1 << 10));
        cpu.direction = self.0.flags & (1 << 10) != 0;
        cpu.alignment_check = self.0.flags & (1 << 18) != 0;
        cpu.id_flag = self.0.flags & (1 << 21) != 0;
        cpu.fs_base = self.0.fs;
        cpu.gs_base = self.0.gs;
        cpu.mxcsr = self.0.mxcsr as u32 & !0x3f | Self::exception_flags(self.0.fpsr);
        for (value, lanes) in cpu.vectors.iter_mut().zip(self.0.vectors.chunks_exact(2)) {
            *value = u128::from(lanes[0]) | u128::from(lanes[1]) << 64;
        }
        for (value, lanes) in cpu.vector_upper.iter_mut().zip(self.0.vector_upper.chunks_exact(2)) {
            *value = u128::from(lanes[0]) | u128::from(lanes[1]) << 64;
        }
    }
}

/// Which step of a projection lease run refused; a bare `Err(())` names none of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LeaseStep {
    Sources = 1,
    StatisticsBefore = 2,
    ActiveView = 3,
    DirectAuthority = 4,
    DirectIdentity = 5,
    DirectRelease = 6,
    Run = 7,
    PublishWritten = 8,
    StatisticsAfter = 9,
    X86Run = 10,
    X86PublishWritten = 11,
    X86StackProjection = 12,
}

/// Unique Rust owner for one opaque native executor instance.
pub(crate) struct Executor {
    handle: NonNull<Handle>,
    memory: Box<ExecutableMemory>,
    diagnostics_enabled: bool,
    fault_owner: Option<std::sync::Arc<dyn HostFaultOwner>>,
    view_hints: std::sync::Mutex<ViewHints>,
    /// Cross-run x86 hint admission, counted only under diagnostics.
    x86_hints_in: std::sync::atomic::AtomicU64,
    x86_hints_accepted: std::sync::atomic::AtomicU64,
    x86_hints_overlap_rejected: std::sync::atomic::AtomicU64,
    x86_hints_subsuming_rejected: std::sync::atomic::AtomicU64,
    x86_hints_unprojectable: std::sync::atomic::AtomicU64,
    /// A lease failure is a fieldless `Err(())`; without this the scheduler cannot
    /// name which step refused.
    lease_failure: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    test_epoch: std::sync::Mutex<Option<(u64, u64)>>,
    #[cfg(test)]
    diagnostic_calls: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    boundary_capture: std::sync::Mutex<BoundaryCaptureState>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BoundarySource {
    guest_first: u64,
    incarnation: u64,
    version: u64,
    bytes: Vec<u8>,
}
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BoundaryView {
    guest_first: u64,
    guest_last: u64,
    permissions: u32,
    bytes: Vec<u8>,
}
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct BoundaryCapture {
    cpu: Aarch64CpuState,
    sources: Vec<BoundarySource>,
    views: Vec<BoundaryView>,
}
#[cfg(test)]
#[derive(Default)]
struct BoundaryCaptureState {
    ordinal: usize,
    maximum: usize,
    used: usize,
    calls: usize,
    result: Option<Result<BoundaryCapture, &'static str>>,
}

#[cfg(test)]
#[derive(Default)]
struct LiveCaptureState {
    ordinal: usize,
    maximum: usize,
    calls: usize,
    result: Option<Result<BoundaryCapture, &'static str>>,
}

#[cfg(test)]
fn live_capture() -> &'static std::sync::Mutex<LiveCaptureState> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<LiveCaptureState>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(LiveCaptureState::default()))
}

#[cfg(test)]
fn arm_live_capture(ordinal: usize, maximum: usize) -> Result<(), &'static str> {
    if ordinal == 0 || maximum == 0 {
        return Err("invalid live capture configuration");
    }
    *live_capture().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = LiveCaptureState {
        ordinal,
        maximum,
        calls: 0,
        result: None,
    };
    Ok(())
}

#[cfg(test)]
fn take_live_capture() -> Option<Result<BoundaryCapture, &'static str>> {
    live_capture()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .result
        .take()
}

#[cfg(test)]
fn begin_live_capture() -> Option<usize> {
    let mut state = live_capture().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.calls = state.calls.saturating_add(1);
    (state.ordinal != 0 && state.calls == state.ordinal).then_some(state.maximum)
}

#[cfg(test)]
fn finish_live_capture(result: Option<Result<BoundaryCapture, &'static str>>) {
    if let Some(result) = result {
        live_capture()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result = Some(result);
    }
}

#[cfg(test)]
impl BoundaryCaptureState {
    fn append_source(&mut self, guest_first: u64, bytes: &[u8], token: ExecutableToken) {
        let Some(Ok(capture)) = self.result.as_mut() else {
            return;
        };
        if capture.sources.iter().any(|source| {
            source.guest_first == guest_first
                && source.incarnation == token.incarnation
                && source.version == token.version
                && source.bytes == bytes
        }) {
            return;
        }
        let Some(guest_last) = guest_first.checked_add(bytes.len() as u64) else {
            self.result = Some(Err("boundary capture source overflow"));
            return;
        };
        if let Some(existing) = capture.sources.iter_mut().find(|source| {
            source.incarnation == token.incarnation
                && source.version == token.version
                && source.guest_first <= guest_last
                && guest_first <= source.guest_first + source.bytes.len() as u64
        }) {
            let first = existing.guest_first.min(guest_first);
            let last = (existing.guest_first + existing.bytes.len() as u64).max(guest_last);
            let length = usize::try_from(last - first).unwrap();
            let Some(used) = self.used.checked_add(length - existing.bytes.len()) else {
                self.result = Some(Err("boundary capture size overflow"));
                return;
            };
            if used > self.maximum {
                self.result = Some(Err("boundary capture size limit"));
                return;
            }
            let mut merged = vec![0; length];
            let old = usize::try_from(existing.guest_first - first).unwrap();
            merged[old..old + existing.bytes.len()].copy_from_slice(&existing.bytes);
            let new = usize::try_from(guest_first - first).unwrap();
            let overlap_first = existing.guest_first.max(guest_first);
            let overlap_last = (existing.guest_first + existing.bytes.len() as u64).min(guest_last);
            if overlap_first < overlap_last {
                let old_offset = usize::try_from(overlap_first - existing.guest_first).unwrap();
                let new_offset = usize::try_from(overlap_first - guest_first).unwrap();
                let overlap = usize::try_from(overlap_last - overlap_first).unwrap();
                if existing.bytes[old_offset..old_offset + overlap] != bytes[new_offset..new_offset + overlap] {
                    self.result = Some(Err("boundary capture source changed"));
                    return;
                }
            }
            merged[new..new + bytes.len()].copy_from_slice(bytes);
            existing.guest_first = first;
            existing.bytes = merged;
            self.used = used;
            return;
        }
        let Some(used) = self.used.checked_add(bytes.len()) else {
            self.result = Some(Err("boundary capture size overflow"));
            return;
        };
        if used > self.maximum || capture.sources.len() == 8 {
            self.result = Some(Err("boundary capture size limit"));
            return;
        }
        self.used = used;
        capture.sources.push(BoundarySource {
            guest_first,
            incarnation: token.incarnation,
            version: token.version,
            bytes: bytes.to_vec(),
        });
    }

    fn append_view(&mut self, view: &ProjectionView) {
        let Some(Ok(capture)) = self.result.as_mut() else {
            return;
        };
        if let Some(existing) = capture
            .views
            .iter_mut()
            .find(|existing| existing.guest_first == view.guest_first && existing.guest_last == view.guest_last)
        {
            existing.permissions |= view.permissions;
            return;
        }
        let value = (|| {
            let length = usize::try_from(
                view.guest_last
                    .checked_sub(view.guest_first)
                    .ok_or("boundary capture invalid view")?,
            )
            .map_err(|_| "boundary capture view overflow")?;
            let used = self.used.checked_add(length).ok_or("boundary capture size overflow")?;
            if used > self.maximum {
                return Err("boundary capture size limit");
            }
            let address = usize::try_from(view.host_first).map_err(|_| "boundary capture host overflow")?;
            let pointer = NonNull::<u8>::new(address as *mut u8).ok_or("boundary capture null host view")?;
            // SAFETY: entry views and slow-miss views are both backed by the live projection lease;
            // the callback copy completes before native execution can resume and mutate the view.
            let bytes = unsafe { std::slice::from_raw_parts(pointer.as_ptr(), length) }.to_vec();
            Ok((
                used,
                BoundaryView {
                    guest_first: view.guest_first,
                    guest_last: view.guest_last,
                    permissions: view.permissions,
                    bytes,
                },
            ))
        })();
        match value {
            Ok((used, view)) => {
                self.used = used;
                if let Some(Ok(capture)) = self.result.as_mut() {
                    capture.views.push(view);
                }
            }
            Err(error) => self.result = Some(Err(error)),
        }
    }
}

impl Executor {
    fn refuse(&self, step: LeaseStep) {
        self.lease_failure
            .store(step as u32, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn last_lease_failure(&self) -> u32 {
        self.lease_failure.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn direct_scalar_trace(&self, pc: u64, sources: &[BorrowedSource<'_>]) -> Option<Protection> {
        let mut protection = None;
        // A run chains single-instruction blocks across branches, so the window is
        // not bounded by the entry block's terminator and may not stop at one.
        for index in 0..64 {
            let Some(instruction) = pc.checked_add(index * 4) else {
                continue;
            };
            let Some(word) = sources.iter().find_map(|source| source.instruction_at(instruction)) else {
                continue;
            };
            // Direct authority runs without the operand resolver, so a vector
            // access it cannot certify would fall back and suppress the entry.
            if word.vector_access() {
                return None;
            }
            let Some(access) = word.scalar_access() else {
                continue;
            };
            // Authority narrower than the window refuses the other direction's
            // accesses outright, so admit their union rather than the last one.
            protection = Some(protection.map_or(access, |seen: Protection| seen.union(access)));
        }
        protection
    }

    #[cfg(test)]
    pub(crate) fn create() -> Result<Self, ()> {
        Self::create_diagnostics(false)
    }

    #[cfg(test)]
    pub(crate) fn create_diagnostics(diagnostics_enabled: bool) -> Result<Self, ()> {
        Self::create_with_fault_owner(diagnostics_enabled, None)
    }

    pub(crate) fn create_with_fault_owner(
        diagnostics_enabled: bool,
        fault_owner: Option<std::sync::Arc<dyn HostFaultOwner>>,
    ) -> Result<Self, ()> {
        Self::create_with_journal(diagnostics_enabled, true, true, fault_owner)
    }

    /// `write_journal` false drops the aarch64 exact dirty journal from every emitted
    /// store, so each crossing publishes the whole window instead of its exact ranges.
    pub(crate) fn create_with_journal(
        diagnostics_enabled: bool,
        write_reserve: bool,
        write_commit: bool,
        fault_owner: Option<std::sync::Arc<dyn HostFaultOwner>>,
    ) -> Result<Self, ()> {
        let mut memory = Box::new(ExecutableMemory::new());
        let services = MemoryServices {
            abi: ABI,
            size: std::mem::size_of::<MemoryServices>() as u32,
            context: std::ptr::from_mut(memory.as_mut()).cast(),
            reserve,
            release,
            publish,
            repair,
            write_begin,
            write_end,
        };
        let config = Config {
            abi: ABI,
            size: std::mem::size_of::<Config>() as u32,
            capacity: 64 << 20,
            alignment: 4096,
            flags: (if cfg!(target_os = "linux") { 2 } else { 0 })
                | if diagnostics_enabled { 4 } else { 0 }
                | if write_reserve { 0 } else { 8 }
                | if write_commit { 0 } else { 16 },
            reserved: 0,
            memory: &raw const services,
        };
        let mut raw = std::ptr::null_mut();
        // SAFETY: `config` and the `services` table it points at are live for this call and
        // are copied by the engine, while the callback context stays valid afterwards
        // because it is the heap body of `memory`, which is moved into `Self` unchanged.
        if unsafe { hl_native_create(&raw const config, &raw mut raw) } != 0 {
            return Err(());
        }
        Ok(Self {
            handle: NonNull::new(raw).ok_or(())?,
            memory,
            diagnostics_enabled,
            fault_owner,
            view_hints: std::sync::Mutex::new(ViewHints::default()),
            x86_hints_in: std::sync::atomic::AtomicU64::new(0),
            x86_hints_accepted: std::sync::atomic::AtomicU64::new(0),
            x86_hints_overlap_rejected: std::sync::atomic::AtomicU64::new(0),
            x86_hints_subsuming_rejected: std::sync::atomic::AtomicU64::new(0),
            x86_hints_unprojectable: std::sync::atomic::AtomicU64::new(0),
            lease_failure: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            test_epoch: std::sync::Mutex::new(None),
            #[cfg(test)]
            diagnostic_calls: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            boundary_capture: std::sync::Mutex::new(BoundaryCaptureState::default()),
        })
    }

    #[cfg(test)]
    fn arm_boundary_capture(&self, ordinal: usize, maximum: usize) -> Result<(), &'static str> {
        if ordinal == 0 || maximum == 0 {
            return Err("invalid boundary capture configuration");
        }
        *self
            .boundary_capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = BoundaryCaptureState {
            ordinal,
            maximum,
            used: 0,
            calls: 0,
            result: None,
        };
        Ok(())
    }

    #[cfg(test)]
    fn take_boundary_capture(&self) -> Option<Result<BoundaryCapture, &'static str>> {
        self.boundary_capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result
            .take()
    }

    #[cfg(test)]
    fn capture_boundary(
        &self,
        cpu: &Aarch64CpuState,
        sources: &[BorrowedSource<'_>],
        views: &[ProjectionView],
        token: ExecutableToken,
    ) {
        let mut state = self
            .boundary_capture
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.calls = state.calls.saturating_add(1);
        if state.ordinal == 0 || state.calls != state.ordinal || state.result.is_some() {
            return;
        }
        let captured = Ok(BoundaryCapture {
            cpu: cpu.clone(),
            sources: Vec::new(),
            views: Vec::new(),
        });
        state.result = Some(captured);
        for source in sources {
            state.append_source(source.guest_first, source.bytes, token);
        }
        for view in views {
            state.append_view(view);
        }
    }

    pub(crate) fn register_direct<'executor, 'memory, H: MemoryAccessHost>(
        &'executor self,
        lease: DirectAuthorityLease<'memory, H>,
    ) -> Result<DirectAuthority<'executor, 'memory, H>, ()> {
        let range = lease.range();
        let generation = lease.generation();
        let descriptor = DirectDescriptor {
            abi: ABI,
            size: std::mem::size_of::<DirectDescriptor>() as u32,
            permissions: u32::from(lease.protection().bits()),
            reserved: 0,
            guest_first: range.start().get(),
            guest_last: range.end().get(),
            host_first: lease.storage_address(),
            mapping_incarnation: generation.incarnation,
            mapping_generation: generation.mappings,
            instruction_generation: generation.instructions,
        };
        let mut token = std::ptr::null_mut();
        // SAFETY: `&self` keeps the handle alive; `descriptor` and `token` are live locals
        // and the engine copies the descriptor rather than retaining the pointer. The
        // returned token's lease is held by the DirectAuthority that owns it.
        if unsafe { hl_native_direct_register(self.handle.as_ptr(), &raw const descriptor, &raw mut token) } != 0 {
            return Err(());
        }
        Ok(DirectAuthority {
            executor: self,
            token: Some(NonNull::new(token).ok_or(())?),
            lease: Some(lease),
        })
    }

    pub(crate) fn reset(&self, mapping_epoch: u64) -> Result<(), ()> {
        *self
            .view_hints
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ViewHints::default();
        let change = Change {
            abi: ABI,
            size: std::mem::size_of::<Change>() as u32,
            kind: 2,
            reserved: 0,
            first: 0,
            last: 0,
            mapping_epoch,
        };
        // SAFETY: `&self` keeps the handle alive and `change` is one live local matching
        // the count of 1; the engine reads it during the call and retains no pointer.
        (unsafe { hl_native_changed(self.handle.as_ptr(), &raw const change, 1) } == 0)
            .then_some(())
            .ok_or(())
    }

    pub(crate) fn invalidate(&self, first: u64, last: u64, mapping_incarnation: u64) -> Result<(), ()> {
        self.invalidate_ranges(&[(first, last)], mapping_incarnation)
    }

    pub(crate) fn invalidate_ranges(&self, ranges: &[(u64, u64)], mapping_incarnation: u64) -> Result<(), ()> {
        if ranges.is_empty() || ranges.len() > 1024 || ranges.iter().any(|(first, last)| last <= first) {
            return Err(());
        }
        let changes: Vec<_> = ranges
            .iter()
            .map(|&(first, last)| Change {
                abi: ABI,
                size: std::mem::size_of::<Change>() as u32,
                kind: 1,
                reserved: 0,
                first,
                last,
                mapping_epoch: mapping_incarnation,
            })
            .collect();
        // SAFETY: `&self` keeps the handle alive and `changes` outlives the call, supplying
        // exactly `len` initialized entries that the engine reads without retaining.
        (unsafe { hl_native_changed(self.handle.as_ptr(), changes.as_ptr(), changes.len()) } == 0)
            .then_some(())
            .ok_or(())
    }

    fn diagnostics(&self) -> Result<Diagnostics, ()> {
        #[cfg(test)]
        self.diagnostic_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut output = Diagnostics {
            abi: ABI,
            size: std::mem::size_of::<Diagnostics>() as u32,
            capacity: 0,
            used: 0,
            publications: 0,
            write_transitions: 0,
            dual_alias: 0,
            writing: 0,
            cache_lookups: 0,
            cache_hits: 0,
            cache_misses: 0,
            epoch_rejections: 0,
            invalidations: 0,
            live_blocks: 0,
            cache_generation: 0,
            mapping_epoch: 0,
            ibtc_fills: 0,
            ibtc_site_collisions: 0,
            ibtc_shared_collisions: 0,
            boundary_branch: 0,
            boundary_syscall: 0,
            boundary_fallback: 0,
            boundary_yield: 0,
            completed: 0,
            operand_callbacks: 0,
            operand_cache_hits: 0,
            x86_public_exits: 0,
            x86_public_syscalls: 0,
            x86_syscall_vector_dirty: 0,
            a64_guard_fast: 0,
            a64_guard_full: 0,
            a64_guard_fallback: 0,
            a64_dirty_reserved: 0,
            a64_dirty_overflow: 0,
            a64_dirty_committed: 0,
            a64_dirty_merged: 0,
            x86_cold_builds: 0,
            x86_cold_quota_exits: 0,
            relocation_cold_targets: 0,
            relocation_cycles: 0,
            relocation_capacity: 0,
            relocation_invalidations: 0,
            ibtc_site_misses: 0,
            ibtc_shared_misses: 0,
            a64_fallback_guard_read: 0,
            a64_fallback_guard_write: 0,
            a64_fallback_simd_fp: 0,
            a64_fallback_memory: 0,
            a64_fallback_control: 0,
            a64_fallback_other: 0,
            a64_fallback_entry_rejection: 0,
            a64_fallback_generated: 0,
            a64_fallback_call: 0,
            a64_fallback_return: 0,
            a64_fallback_indirect: 0,
            a64_fallback_system: 0,
            a64_fallback_form_memory: 0,
            a64_fallback_form_other: 0,
            x86_public_epochs: 0,
            a64_branch_exhaustion: 0,
            a64_branch_cold_relocation: 0,
            a64_branch_nonrelocatable: 0,
            a64_branch_unidentified: 0,
            a64_branch_sample_pc: 0,
            a64_branch_sample_source_first: 0,
            a64_branch_sample_source_last: 0,
            a64_branch_sample_form: 0,
            ibtc_authenticated_entries: 0,
            ibtc_shared_hits: 0,
            ibtc_auth_rejections: 0,
        };
        // SAFETY: `&self` keeps the handle alive and `output` is a fully initialized local
        // the engine only overwrites; it is not retained past the call.
        (unsafe { hl_native_diagnose(self.handle.as_ptr(), &raw mut output) } == 0)
            .then_some(output)
            .ok_or(())
    }

    fn statistics_snapshot(&self) -> Result<Option<Diagnostics>, ()> {
        self.diagnostics_enabled.then(|| self.diagnostics()).transpose()
    }

    /// Cross-run hint admission is only observable from Rust, so count it here rather
    /// than in the engine's own diagnostics block.
    fn count_x86_hint(&self, counter: &std::sync::atomic::AtomicU64) {
        if self.diagnostics_enabled {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub(crate) fn run_lease<H: MemoryAccessHost>(
        &self,
        state: &mut Aarch64CpuState,
        sources: &[BorrowedSource<'_>],
        mut lease: ProjectionLease<'_, H>,
        token: ExecutableToken,
        interrupt: &InterruptToken,
        budget: u64,
        resolve: &mut dyn FnMut(u64, &mut [u8]) -> Option<(usize, ExecutableToken)>,
        allow_direct: bool,
        mut poll: Option<&mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<(RunOutcome, RunStatistics), ()> {
        #[cfg(feature = "alloc-count")]
        let _crossing = allocations::Crossing::begin();
        if sources.is_empty() || sources.len() > 8 || sources.iter().any(|span| span.bytes.is_empty()) {
            self.refuse(LeaseStep::Sources);
            return Err(());
        }
        #[cfg(test)]
        if let Some(maximum) = begin_live_capture() {
            self.arm_boundary_capture(1, maximum).map_err(|_| ())?;
        }
        let generation = lease.generation();
        let hint_epoch = [generation.incarnation, generation.mappings, token.version, 0];
        let before = self
            .statistics_snapshot()
            .inspect_err(|()| self.refuse(LeaseStep::StatisticsBefore))?;
        let spans: Vec<_> = sources
            .iter()
            .map(|span| SourceSpan {
                guest_first: span.guest_first,
                bytes: span.bytes.as_ptr(),
                size: span.bytes.len(),
                mapping_incarnation: generation.incarnation,
                instruction_epoch: token.version,
            })
            .collect();
        let source = Source {
            spans: spans.as_ptr(),
            span_count: spans.len(),
            mapping_incarnation: generation.incarnation,
            instruction_epoch: token.version,
        };
        let range = lease.range();
        let primary = ProjectionView {
            guest_first: range.start().get(),
            guest_last: range.end().get(),
            host_first: lease.storage_address(),
            mapping_incarnation: generation.incarnation,
            permissions: u32::from(projection_permissions(lease.authority(), lease.protection()).bits()),
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let hints = {
            let mut retained = self
                .view_hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retained.begin(hint_epoch);
            retained.entries.clone()
        };
        let lease_range = (primary.guest_first, primary.guest_last);
        // The authority identity is cache-wide: a per-entry narrowing of the same window
        // reissues it and resets every translation, so admit the whole lease instead.
        // Both decodes are pure reads of `sources`, so skip them when the result is discarded.
        let direct_protection = allow_direct
            .then(|| {
                self.direct_scalar_trace(state.pc, sources)
                    .or_else(|| {
                        direct_literal_target(state.pc, sources, lease_range.0, lease_range.1)
                            .then_some(Protection::READ)
                    })
                    .filter(|required| lease.allows(*required))
            })
            .flatten()
            .map(|_| lease.authority());
        let mut primary_range = lease_range;
        let mut views = vec![primary];
        const OPERAND_SPAN: u64 = u64::MAX;
        let mut live_hints = Vec::new();
        let mut warm_write = None;
        for (address, required) in hints {
            // A hint records one address but projects its whole region, so replaying the
            // narrow observed access publishes a region-wide view that rejects the
            // opposite access. Widen as `resolve_operand` does, narrowing only when the
            // region genuinely lacks the permission.
            let widened = required.union(Protection::READ).union(Protection::WRITE);
            let Ok(view) = lease
                .project_bounded(GuestAddress::new(address), 1, widened, OPERAND_SPAN)
                .or_else(|_| lease.project_bounded(GuestAddress::new(address), 1, required, OPERAND_SPAN))
            else {
                continue;
            };
            let candidate = ProjectionView {
                guest_first: view.range.start().get(),
                guest_last: view.range.end().get(),
                host_first: view.storage_address,
                mapping_incarnation: generation.incarnation,
                permissions: u32::from(view.protection.bits()),
                write_policy: WRITE_EXACT,
                write_index: view.index,
            };
            let candidate_range = (candidate.guest_first, candidate.guest_last);
            let projected = view.protection;
            let overlaps = |existing: &ProjectionView| {
                existing.guest_first < candidate.guest_last && candidate.guest_first < existing.guest_last
            };
            // A hint that subsumes a view under the same translation and permissions
            // replaces it, since every address the narrow view resolved resolves
            // identically through the wider one.
            // The direct authority is issued against the lease window itself, so a
            // widened view there fails the direct synchronize check.
            let subsumed = direct_protection.is_none().then(|| {
                views.iter().position(|existing| {
                    overlaps(existing)
                        && candidate.guest_first <= existing.guest_first
                        && existing.guest_last <= candidate.guest_last
                        && candidate.permissions == existing.permissions
                        && candidate.host_first.wrapping_sub(candidate.guest_first)
                            == existing.host_first.wrapping_sub(existing.guest_first)
                })
            });
            let admitted = match subsumed.flatten() {
                Some(index)
                    if !views
                        .iter()
                        .enumerate()
                        .any(|(other, existing)| other != index && overlaps(existing)) =>
                {
                    let replaced = (views[index].guest_first, views[index].guest_last);
                    views[index] = candidate;
                    if primary_range == replaced {
                        primary_range = candidate_range;
                    }
                    if warm_write == Some(replaced) {
                        warm_write = Some(candidate_range);
                    }
                    true
                }
                _ if !views.iter().any(overlaps) => {
                    views.push(candidate);
                    true
                }
                _ => false,
            };
            if admitted {
                if required.contains(Protection::WRITE) && projected.contains(Protection::WRITE) && warm_write.is_none()
                {
                    warm_write = Some(candidate_range);
                }
                live_hints.push((address, required));
            }
        }
        views.sort_unstable_by_key(|view| view.guest_first);
        let active_range = if direct_protection.is_some() {
            primary_range
        } else {
            warm_write.unwrap_or(primary_range)
        };
        let active = views
            .iter()
            .position(|view| (view.guest_first, view.guest_last) == active_range)
            .ok_or_else(|| self.refuse(LeaseStep::ActiveView))?;
        let projection = Projection {
            views: views.as_ptr(),
            count: views.len(),
            mapping_incarnation: generation.incarnation,
            active,
        };
        #[cfg(test)]
        self.capture_boundary(state, sources, &views, token);
        let mut provider = SourceProvider {
            resolve,
            bytes: [0; 256],
            observed: sources
                .iter()
                .map(|source| {
                    (
                        source.guest_first,
                        source.guest_first + source.bytes.len() as u64,
                        token,
                    )
                })
                .collect(),
            complete: true,
            #[cfg(test)]
            boundary_capture: Some(&self.boundary_capture),
        };
        let mut observed = ViewHints::default();
        let result = if let Some(required) = direct_protection {
            let authority = self.register_direct(
                lease
                    .into_direct(required)
                    .map_err(|_| self.refuse(LeaseStep::DirectAuthority))?,
            )?;
            let identity = authority
                .request_identity()
                .ok_or_else(|| self.refuse(LeaseStep::DirectIdentity))?;
            let result = self.run_aarch64_inner(
                state,
                &source,
                Some(&projection),
                generation.incarnation,
                budget,
                Some(((&raw mut provider).cast(), resolve_source)),
                None,
                Some(interrupt),
                Some(identity),
                poll.as_mut()
                    .map(|callback| &mut **callback as &mut dyn FnMut(u64, u64) -> bool),
            );
            lease = authority
                .into_lease()
                .inspect_err(|()| self.refuse(LeaseStep::DirectRelease))?
                .into_projection();
            result
        } else {
            let mut operand = OperandProvider {
                lease: &mut lease,
                observed: &mut observed,
                #[cfg(test)]
                boundary_capture: Some(&self.boundary_capture),
            };
            self.run_aarch64_inner(
                state,
                &source,
                Some(&projection),
                generation.incarnation,
                budget,
                Some(((&raw mut provider).cast(), resolve_source)),
                Some(((&raw mut operand).cast(), resolve_operand::<H>)),
                Some(interrupt),
                None,
                poll.as_mut()
                    .map(|callback| &mut **callback as &mut dyn FnMut(u64, u64) -> bool),
            )
        };
        let (exit, instruction, _, code, remaining, executed, writes) =
            result.inspect_err(|()| self.refuse(LeaseStep::Run))?;
        #[cfg(test)]
        finish_live_capture(self.take_boundary_capture());
        {
            let mut retained = self
                .view_hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retained.begin(hint_epoch);
            retained.entries.retain(|hint| live_hints.contains(hint));
            for (address, required) in observed.entries.into_iter().rev() {
                retained.observe(address, required);
            }
        }
        match writes {
            NativeWrites::None => {
                #[cfg(feature = "alloc-count")]
                allocations::count(&allocations::WRITES_NONE);
                drop(lease);
            }
            NativeWrites::Exact(ranges) => {
                #[cfg(feature = "alloc-count")]
                allocations::count(&allocations::WRITES_EXACT);
                #[cfg(feature = "alloc-count")]
                let result = allocations::in_publish(|| lease.publish_written_ranges(&ranges));
                #[cfg(not(feature = "alloc-count"))]
                let result = lease.publish_written_ranges(&ranges);
                result.map_err(|_| self.refuse(LeaseStep::PublishWritten))?;
            }
            NativeWrites::Full => {
                #[cfg(feature = "alloc-count")]
                allocations::count(&allocations::WRITES_FULL);
                #[cfg(feature = "alloc-count")]
                let result = allocations::in_publish(|| lease.publish_written());
                #[cfg(not(feature = "alloc-count"))]
                let result = lease.publish_written();
                result.map_err(|_| self.refuse(LeaseStep::PublishWritten))?;
            }
        }
        let after = self
            .statistics_snapshot()
            .inspect_err(|()| self.refuse(LeaseStep::StatisticsAfter))?;
        Ok((
            RunOutcome {
                exit,
                instruction,
                code,
                remaining,
                executed,
            },
            RunStatistics {
                builds: after.zip(before).map_or(0, |(after, before)| {
                    after.publications.saturating_sub(before.publications)
                }),
                hits: after
                    .zip(before)
                    .map_or(0, |(after, before)| after.cache_hits.saturating_sub(before.cache_hits)),
                fallback: exit == Exit::Fallback,
                direct_guard: direct_protection.is_some()
                    && exit == Exit::Fallback
                    && sources
                        .iter()
                        .find_map(|source| source.instruction_at(instruction))
                        .is_some_and(|word| word.scalar_access().is_some()),
                direct: direct_protection.is_some(),
                sources: provider.observed,
                sources_complete: provider.complete,
            },
        ))
    }

    #[cfg(test)]
    pub(crate) fn run_aarch64(
        &self,
        state: &mut Aarch64CpuState,
        source: &Source,
        projection: Option<&Projection>,
        mapping_epoch: u64,
        budget: u64,
        provider: Option<(*mut c_void, SourceResolve)>,
        operand: Option<(*mut c_void, OperandResolve)>,
    ) -> Result<(Exit, u64, u64, u64, u64, u64, NativeWrites), ()> {
        self.run_aarch64_inner(
            state,
            source,
            projection,
            mapping_epoch,
            budget,
            provider,
            operand,
            None,
            None,
            None,
        )
    }

    fn run_aarch64_inner(
        &self,
        state: &mut Aarch64CpuState,
        source: &Source,
        projection: Option<&Projection>,
        mapping_epoch: u64,
        budget: u64,
        provider: Option<(*mut c_void, SourceResolve)>,
        operand: Option<(*mut c_void, OperandResolve)>,
        interrupt: Option<&InterruptToken>,
        direct: Option<(*const DirectToken, u64, u64)>,
        mut poll: Option<&mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<(Exit, u64, u64, u64, u64, u64, NativeWrites), ()> {
        let (quantum_context, quantum_poll, quantum_grant) = match poll.as_mut() {
            Some(callback) => (
                std::ptr::from_mut::<&mut dyn FnMut(u64, u64) -> bool>(callback).cast(),
                Some(poll_quantum as unsafe extern "C" fn(*mut c_void, u64, u64) -> u32),
                budget,
            ),
            None => (std::ptr::null_mut(), None, 0),
        };
        let mut native = NativeAarch64::capture(state);
        native.0.interrupt_token = interrupt.map_or(0, InterruptToken::as_raw);
        let mut cpu = CpuHandle {
            abi: ABI,
            size: std::mem::size_of::<CpuHandle>() as u32,
            architecture: AARCH64,
            reserved: 0,
            state: (&raw mut native.0).cast(),
        };
        let (source_context, source_resolve) = provider.unzip();
        let (operand_context, operand_resolve) = operand.unzip();
        let request = RunRequest {
            abi: ABI,
            size: std::mem::size_of::<RunRequest>() as u32,
            architecture: AARCH64,
            reserved: 0,
            mapping_epoch,
            budget,
            source,
            projection: projection.map_or(std::ptr::null(), std::ptr::from_ref),
            source_context: source_context.unwrap_or(std::ptr::null_mut()),
            source_resolve,
            operand_context: operand_context.unwrap_or(std::ptr::null_mut()),
            operand_resolve,
            fault_context: self.fault_owner.as_ref().map_or(std::ptr::null_mut(), |owner| {
                std::ptr::from_ref(owner).cast_mut().cast()
            }),
            fault_publish: self.fault_owner.as_ref().map(|_| fault_publish as _),
            fault_unpublish: self.fault_owner.as_ref().map(|_| fault_unpublish as _),
            memory_mode: u64::from(direct.is_some()),
            authority_generation: direct.map_or(0, |identity| identity.1),
            direct_token: direct.map_or(std::ptr::null(), |identity| identity.0),
            authority_identity: direct.map_or(0, |identity| identity.2),
            quantum_context,
            quantum_poll,
            quantum_grant,
            certificate: std::ptr::null(),
        };
        let mut output = RunExit {
            abi: ABI,
            size: std::mem::size_of::<RunExit>() as u32,
            kind: 0,
            access: 0,
            instruction: 0,
            next: 0,
            address: 0,
            code: 0,
        };
        // SAFETY: every C view is repr(C), borrowed for this call, and the owned
        // executor excludes destruction while `self` is borrowed.
        let status = unsafe { hl_native_run(self.handle.as_ptr(), &raw mut cpu, &raw const request, &raw mut output) };
        if status != 0 {
            if self.diagnostics_enabled {
                eprintln!(
                    "hl-native-error: isa=aarch64 status={status} pc={:#x} invariant={}",
                    native.0.program,
                    state_invariant()
                );
            }
            return Err(());
        }
        native.restore(state);
        Ok((
            Exit::try_from(output.kind)?,
            output.instruction,
            output.address,
            output.code,
            native.0.budget,
            native.0.executed,
            native.writes(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn run_x86(
        &self,
        state: &mut X86CpuState,
        sources: &[BorrowedSource<'_>],
        mapping_epoch: u64,
        instruction_epoch: u64,
        budget: u64,
        interrupt: bool,
        projection: Option<&Projection>,
        resolve: &mut dyn FnMut(u64, &mut [u8]) -> Option<usize>,
    ) -> Result<(X86RunOutcome, RunStatistics, bool), ()> {
        self.run_x86_test(
            state,
            sources,
            mapping_epoch,
            instruction_epoch,
            budget,
            interrupt,
            projection,
            resolve,
            None,
        )
    }

    #[cfg(test)]
    fn run_x86_test(
        &self,
        state: &mut X86CpuState,
        sources: &[BorrowedSource<'_>],
        mapping_epoch: u64,
        instruction_epoch: u64,
        budget: u64,
        interrupt: bool,
        projection: Option<&Projection>,
        resolve: &mut dyn FnMut(u64, &mut [u8]) -> Option<usize>,
        poll: Option<&mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<(X86RunOutcome, RunStatistics, bool), ()> {
        let mut observed = self.test_epoch.lock().map_err(|_| ())?;
        if let Some((previous_mapping, previous_instruction)) = *observed {
            if previous_mapping != mapping_epoch {
                self.reset(mapping_epoch)?;
            } else if previous_instruction != instruction_epoch {
                for source in sources {
                    self.invalidate(
                        source.guest_first,
                        source.guest_first.checked_add(source.bytes.len() as u64).ok_or(())?,
                        mapping_epoch,
                    )?;
                }
            }
        }
        *observed = Some((mapping_epoch, instruction_epoch));
        drop(observed);
        let mut tagged = |address, output: &mut [u8]| {
            resolve(address, output).map(|size| {
                (
                    size,
                    ExecutableToken {
                        incarnation: mapping_epoch,
                        version: instruction_epoch,
                    },
                )
            })
        };
        self.run_x86_inner(
            state,
            sources,
            mapping_epoch,
            instruction_epoch,
            budget,
            interrupt,
            projection,
            &mut tagged,
            None,
            poll,
        )
        .map(|(outcome, statistics, writes)| (outcome, statistics, !matches!(writes, NativeWrites::None)))
    }

    pub(crate) fn run_x86_lease<H: MemoryAccessHost>(
        &self,
        state: &mut X86CpuState,
        sources: &[BorrowedSource<'_>],
        mut lease: ProjectionLease<'_, H>,
        token: ExecutableToken,
        budget: u64,
        interrupt: bool,
        resolve: &mut dyn FnMut(u64, &mut [u8]) -> Option<(usize, ExecutableToken)>,
        poll: Option<&mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<(X86RunOutcome, RunStatistics), ()> {
        #[cfg(feature = "alloc-count")]
        let _crossing = allocations::Crossing::begin();
        let generation = lease.generation();
        let range = lease.range();
        let primary = ProjectionView {
            guest_first: range.start().get(),
            guest_last: range.end().get(),
            host_first: lease.storage_address(),
            mapping_incarnation: generation.incarnation,
            permissions: view_permissions(
                &lease,
                projection_permissions(lease.authority(), lease.protection()),
                range.start(),
            ),
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let hint_epoch = [generation.incarnation, generation.mappings, token.version, 0];
        let hints = {
            let mut retained = self
                .view_hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retained.begin(hint_epoch);
            retained.entries.clone()
        };
        let primary_range = (primary.guest_first, primary.guest_last);
        let mut views = vec![primary];
        const OPERAND_SPAN: u64 = u64::MAX;
        for (address, required) in hints {
            self.count_x86_hint(&self.x86_hints_in);
            // A hint records one address but projects its whole region, so replaying the
            // narrow observed access publishes a region-wide view that rejects the
            // opposite access. Widen as `resolve_operand` does, narrowing only when the
            // region genuinely lacks the permission.
            let widened = required.union(Protection::READ).union(Protection::WRITE);
            let Ok(view) = lease
                .project_bounded(hl_isa::GuestAddress::new(address), 1, widened, OPERAND_SPAN)
                .or_else(|_| lease.project_bounded(hl_isa::GuestAddress::new(address), 1, required, OPERAND_SPAN))
            else {
                self.count_x86_hint(&self.x86_hints_unprojectable);
                continue;
            };
            let candidate = ProjectionView {
                guest_first: view.range.start().get(),
                guest_last: view.range.end().get(),
                host_first: view.storage_address,
                mapping_incarnation: generation.incarnation,
                permissions: view_permissions(&lease, view.protection, view.range.start()),
                write_policy: WRITE_EXACT,
                write_index: view.index,
            };
            let overlaps = |existing: &ProjectionView| {
                existing.guest_first < candidate.guest_last && candidate.guest_first < existing.guest_last
            };
            if views.iter().any(overlaps) {
                self.count_x86_hint(&self.x86_hints_overlap_rejected);
                // Whether the aarch64 subsumption rule would have admitted this one: a
                // candidate covering a lone view under the same host delta and permissions
                // resolves every address that view resolved.
                if views.iter().enumerate().any(|(index, existing)| {
                    overlaps(existing)
                        && candidate.guest_first <= existing.guest_first
                        && existing.guest_last <= candidate.guest_last
                        && candidate.permissions == existing.permissions
                        && candidate.host_first.wrapping_sub(candidate.guest_first)
                            == existing.host_first.wrapping_sub(existing.guest_first)
                        && !views
                            .iter()
                            .enumerate()
                            .any(|(other, view)| other != index && overlaps(view))
                }) {
                    self.count_x86_hint(&self.x86_hints_subsuming_rejected);
                }
            } else {
                self.count_x86_hint(&self.x86_hints_accepted);
                views.push(candidate);
            }
        }
        views.sort_unstable_by_key(|view| view.guest_first);
        let active = views
            .iter()
            .position(|view| (view.guest_first, view.guest_last) == primary_range)
            .ok_or(())?;
        let projection = Projection {
            views: views.as_ptr(),
            count: views.len(),
            mapping_incarnation: generation.incarnation,
            active,
        };
        let mut observed = ViewHints::default();
        let mut operand = OperandProvider {
            lease: &mut lease,
            observed: &mut observed,
            #[cfg(test)]
            boundary_capture: None,
        };
        let (outcome, statistics, written) = self.run_x86_inner(
            state,
            sources,
            generation.incarnation,
            token.version,
            budget,
            interrupt,
            Some(&projection),
            resolve,
            Some(((&raw mut operand).cast(), resolve_operand::<H>)),
            poll,
        )?;
        if !observed.entries.is_empty() {
            let mut retained = self
                .view_hints
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retained.begin(hint_epoch);
            for (address, required) in observed.entries.into_iter().rev() {
                retained.observe(address, required);
            }
        }
        match written {
            NativeWrites::None => drop(lease),
            NativeWrites::Exact(ranges) => lease
                .publish_written_ranges(&ranges)
                .map_err(|_| self.refuse(LeaseStep::X86PublishWritten))?,
            NativeWrites::Full => lease
                .publish_written()
                .map_err(|_| self.refuse(LeaseStep::X86PublishWritten))?,
        }
        Ok((outcome, statistics))
    }

    fn run_x86_inner(
        &self,
        state: &mut X86CpuState,
        sources: &[BorrowedSource<'_>],
        mapping_epoch: u64,
        instruction_epoch: u64,
        budget: u64,
        interrupt: bool,
        projection: Option<&Projection>,
        resolve: &mut dyn FnMut(u64, &mut [u8]) -> Option<(usize, ExecutableToken)>,
        operand: Option<(*mut c_void, OperandResolve)>,
        poll: Option<&mut dyn FnMut(u64, u64) -> bool>,
    ) -> Result<(X86RunOutcome, RunStatistics, NativeWrites), ()> {
        if sources.is_empty() || sources.len() > 8 || sources.iter().any(|span| span.bytes.is_empty()) {
            self.refuse(LeaseStep::Sources);
            return Err(());
        }
        let before = self
            .statistics_snapshot()
            .inspect_err(|()| self.refuse(LeaseStep::StatisticsBefore))?;
        let spans: Vec<_> = sources
            .iter()
            .map(|span| SourceSpan {
                guest_first: span.guest_first,
                bytes: span.bytes.as_ptr(),
                size: span.bytes.len(),
                mapping_incarnation: mapping_epoch,
                instruction_epoch,
            })
            .collect();
        let source = Source {
            spans: spans.as_ptr(),
            span_count: spans.len(),
            mapping_incarnation: mapping_epoch,
            instruction_epoch,
        };
        let token = ExecutableToken {
            incarnation: mapping_epoch,
            version: instruction_epoch,
        };
        let mut provider = SourceProvider {
            resolve,
            bytes: [0; 256],
            observed: sources
                .iter()
                .map(|source| {
                    (
                        source.guest_first,
                        source.guest_first + source.bytes.len() as u64,
                        token,
                    )
                })
                .collect(),
            complete: true,
            #[cfg(test)]
            boundary_capture: None,
        };
        let (operand_context, operand_resolve) = operand.unzip();
        let mut poll = poll;
        let (quantum_context, quantum_poll, quantum_grant) = match poll.as_mut() {
            Some(callback) => (
                std::ptr::from_mut::<&mut dyn FnMut(u64, u64) -> bool>(callback).cast(),
                Some(poll_quantum as unsafe extern "C" fn(*mut c_void, u64, u64) -> u32),
                budget,
            ),
            None => (std::ptr::null_mut(), None, 0),
        };
        let mut native = NativeX86::capture(state, interrupt);
        let mut cpu = CpuHandle {
            abi: ABI,
            size: std::mem::size_of::<CpuHandle>() as u32,
            architecture: X86_64,
            reserved: 0,
            state: (&raw mut native.0).cast(),
        };
        let request = RunRequest {
            abi: ABI,
            size: std::mem::size_of::<RunRequest>() as u32,
            architecture: X86_64,
            reserved: 0,
            mapping_epoch,
            budget,
            source: &raw const source,
            projection: projection.map_or(std::ptr::null(), std::ptr::from_ref),
            source_context: (&raw mut provider).cast(),
            source_resolve: Some(resolve_source),
            operand_context: operand_context.unwrap_or(std::ptr::null_mut()),
            operand_resolve,
            fault_context: std::ptr::null_mut(),
            fault_publish: None,
            fault_unpublish: None,
            memory_mode: 0,
            authority_generation: 0,
            direct_token: std::ptr::null(),
            authority_identity: 0,
            quantum_context,
            quantum_poll,
            quantum_grant,
            certificate: std::ptr::null(),
        };
        let mut output = RunExit {
            abi: ABI,
            size: std::mem::size_of::<RunExit>() as u32,
            kind: 0,
            access: 0,
            instruction: 0,
            next: 0,
            address: 0,
            code: 0,
        };
        // SAFETY: as in the aarch64 path -- `cpu`, `request` and `output` are repr(C)
        // locals borrowed for exactly this call, and the `&self` borrow excludes executor
        // destruction; every context pointer inside `request` outlives the run.
        if unsafe { hl_native_run(self.handle.as_ptr(), &raw mut cpu, &raw const request, &raw mut output) } != 0 {
            self.refuse(LeaseStep::X86Run);
            return Err(());
        }
        native.restore(state);
        let exit = Exit::try_from(output.kind).inspect_err(|()| self.refuse(LeaseStep::X86Run))?;
        let after = self
            .statistics_snapshot()
            .inspect_err(|()| self.refuse(LeaseStep::StatisticsAfter))?;
        Ok((
            X86RunOutcome {
                exit,
                instruction: output.instruction,
                next: output.next,
                address: output.address,
                code: output.code,
                remaining: native.0.budget,
                executed: native.0.executed,
            },
            RunStatistics {
                builds: after.zip(before).map_or(0, |(after, before)| {
                    after.publications.saturating_sub(before.publications)
                }),
                hits: after
                    .zip(before)
                    .map_or(0, |(after, before)| after.cache_hits.saturating_sub(before.cache_hits)),
                fallback: exit == Exit::Fallback,
                direct_guard: false,
                direct: false,
                sources: provider.observed,
                sources_complete: provider.complete,
            },
            native.writes(),
        ))
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        // Process-wide totals, so read the last line an executor prints.
        #[cfg(feature = "alloc-count")]
        eprintln!(
            "hl-native-alloc: crossings={} allocations={} writes_none={} writes_exact={} writes_full={} publish_allocations={}",
            allocations::CROSSINGS.load(std::sync::atomic::Ordering::Relaxed),
            allocations::ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed),
            allocations::WRITES_NONE.load(std::sync::atomic::Ordering::Relaxed),
            allocations::WRITES_EXACT.load(std::sync::atomic::Ordering::Relaxed),
            allocations::WRITES_FULL.load(std::sync::atomic::Ordering::Relaxed),
            allocations::PUBLISH_ALLOCATIONS.load(std::sync::atomic::Ordering::Relaxed),
        );
        if self.diagnostics_enabled
            && let Ok(value) = self.diagnostics()
        {
            eprintln!(
                "hl-native-detail: fills={} site_collisions={} shared_collisions={} branch={} syscall={} fallback={} yield={} completed={} operand_callbacks={} operand_cache_hits={} x86_public_exits={} x86_public_syscalls={} x86_public_epochs={} x86_syscall_vector_dirty={} x86_cold_builds={} x86_cold_quota_exits={} a64_guard_fast={} a64_guard_full={} a64_guard_fallback={} a64_dirty_reserved={} a64_dirty_overflow={} a64_dirty_committed={} a64_dirty_merged={} relocation_cold_targets={} relocation_cycles={} relocation_capacity={} relocation_invalidations={} ibtc_site_misses={} ibtc_shared_misses={} a64_fallback_guard_read={} a64_fallback_guard_write={} a64_fallback_simd_fp={} a64_fallback_memory={} a64_fallback_control={} a64_fallback_other={} a64_fallback_entry_rejection={} a64_fallback_generated={} a64_fallback_call={} a64_fallback_return={} a64_fallback_indirect={} a64_fallback_system={} a64_fallback_form_memory={} a64_fallback_form_other={} ibtc_authenticated_entries={} ibtc_shared_hits={} ibtc_auth_rejections={} a64_slim_exits=0 a64_branch_exhaustion={} a64_branch_cold_relocation={} a64_branch_nonrelocatable={} a64_branch_unidentified={} a64_branch_sample_pc={:#x} a64_branch_sample_source_first={:#x} a64_branch_sample_source_last={:#x} a64_branch_sample_form={} x86_hints_in={} x86_hints_accepted={} x86_hints_overlap_rejected={} x86_hints_subsuming_rejected={} x86_hints_unprojectable={}",
                value.ibtc_fills,
                value.ibtc_site_collisions,
                value.ibtc_shared_collisions,
                value.boundary_branch,
                value.boundary_syscall,
                value.boundary_fallback,
                value.boundary_yield,
                value.completed,
                value.operand_callbacks,
                value.operand_cache_hits,
                value.x86_public_exits,
                value.x86_public_syscalls,
                value.x86_public_epochs,
                value.x86_syscall_vector_dirty,
                value.x86_cold_builds,
                value.x86_cold_quota_exits,
                value.a64_guard_fast,
                value.a64_guard_full,
                value.a64_guard_fallback,
                value.a64_dirty_reserved,
                value.a64_dirty_overflow,
                value.a64_dirty_committed,
                value.a64_dirty_merged,
                value.relocation_cold_targets,
                value.relocation_cycles,
                value.relocation_capacity,
                value.relocation_invalidations,
                value.ibtc_site_misses,
                value.ibtc_shared_misses,
                value.a64_fallback_guard_read,
                value.a64_fallback_guard_write,
                value.a64_fallback_simd_fp,
                value.a64_fallback_memory,
                value.a64_fallback_control,
                value.a64_fallback_other,
                value.a64_fallback_entry_rejection,
                value.a64_fallback_generated,
                value.a64_fallback_call,
                value.a64_fallback_return,
                value.a64_fallback_indirect,
                value.a64_fallback_system,
                value.a64_fallback_form_memory,
                value.a64_fallback_form_other,
                value.ibtc_authenticated_entries,
                value.ibtc_shared_hits,
                value.ibtc_auth_rejections,
                value.a64_branch_exhaustion,
                value.a64_branch_cold_relocation,
                value.a64_branch_nonrelocatable,
                value.a64_branch_unidentified,
                value.a64_branch_sample_pc,
                value.a64_branch_sample_source_first,
                value.a64_branch_sample_source_last,
                value.a64_branch_sample_form,
                self.x86_hints_in.load(std::sync::atomic::Ordering::Relaxed),
                self.x86_hints_accepted.load(std::sync::atomic::Ordering::Relaxed),
                self.x86_hints_overlap_rejected
                    .load(std::sync::atomic::Ordering::Relaxed),
                self.x86_hints_subsuming_rejected
                    .load(std::sync::atomic::Ordering::Relaxed),
                self.x86_hints_unprojectable.load(std::sync::atomic::Ordering::Relaxed),
            );
        }
        // SAFETY: construction transfers unique ownership of the live handle;
        // Drop runs once and the owner contract excludes admitted executions.
        let status = unsafe { hl_native_destroy(self.handle.as_ptr()) };
        debug_assert_eq!(status, 0, "unique executor drop had an active native lease");
        debug_assert!(self.memory.writable.is_null());
    }
}

/// Names the invariant behind this thread's most recent `HL_NATIVE_STATE`.
pub(crate) fn state_invariant() -> &'static str {
    // SAFETY: the callee returns a static NUL-terminated C string for this thread.
    unsafe { std::ffi::CStr::from_ptr(hl_native_state_invariant()) }
        .to_str()
        .unwrap_or("unclassified")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Exit {
    Branch,
    Syscall,
    Fallback,
    Fault,
    Interrupt,
    Epoch,
    Yield,
    Fatal,
}

impl TryFrom<u32> for Exit {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Branch),
            2 => Ok(Self::Syscall),
            3 => Ok(Self::Fallback),
            4 => Ok(Self::Fault),
            5 => Ok(Self::Interrupt),
            6 => Ok(Self::Epoch),
            7 => Ok(Self::Yield),
            8 => Ok(Self::Fatal),
            _ => Err(()),
        }
    }
}

const _: () = {
    assert!(std::mem::size_of::<SourceSpan>() == 40);
    assert!(std::mem::size_of::<Source>() == 32);
    assert!(std::mem::size_of::<ProjectionView>() == 40);
    assert!(std::mem::size_of::<Projection>() == 32);
    assert!(std::mem::size_of::<RunCertificate>() == 112);
    assert!(std::mem::offset_of!(RunCertificate, direct_token) == 104);
    assert!(std::mem::offset_of!(RunRequest, certificate) == 160);
    assert!(std::mem::size_of::<RunRequest>() == 168);
    assert!(std::mem::size_of::<CpuHandle>() == 24);
    assert!(std::mem::size_of::<FaultScope>() == 32);
    assert!(std::mem::size_of::<RunExit>() == 48);
    assert!(std::mem::size_of::<Change>() == 40);
    assert!(std::mem::offset_of!(Diagnostics, ibtc_authenticated_entries) == 520);
    assert!(std::mem::size_of::<Diagnostics>() == 544);
};

#[cfg(test)]
mod test {
    use super::*;

    /// Two mappings of one shared object, as a guest gets from a memfd mapped RW and RX:
    /// a store through the writable alias modifies code, so its view must carry EXECUTE.
    #[test]
    fn writable_alias_of_an_executable_object_projects_execute() {
        use std::sync::Arc;

        use hl_memory::{Backing, MapRequest, MappingCoordinator, Placement, SharedBackingRef};

        use crate::ffi::linux::{MappingHostAdapter, shared_backed_arena};

        let (shared, arena) = shared_backed_arena(0x4000);
        let object = shared.create(7, 8192).unwrap();
        let memory = MappingCoordinator::with_shared(MappingHostAdapter::new(Arc::new(arena)), shared);
        let request = |address: u64, offset: u64, protection: Protection| MapRequest {
            placement: Placement::Fixed(GuestAddress::new(address)),
            length: 4096,
            alignment: 4096,
            protection,
            backing: Backing::Shared(SharedBackingRef {
                object,
                offset,
                length: 4096,
                write_shared: true,
            }),
            backing_offset: 0,
        };
        let writable = Protection::READ.union(Protection::WRITE);
        let executable = Protection::READ.union(Protection::EXECUTE);
        memory.map(request(0x0000, 0, writable)).unwrap();

        let published = |lease: &ProjectionLease<'_, _>| {
            view_permissions(
                lease,
                projection_permissions(lease.authority(), lease.protection()),
                GuestAddress::ZERO,
            )
        };
        let project = || memory.project_contiguous(GuestAddress::ZERO, 16, writable, 1).unwrap();
        assert_eq!(published(&project()), u32::from(writable.bits()));

        // An image maps its text and its data from one object at disjoint offsets, which
        // shares a backing identity without aliasing any byte.
        memory.map(request(0x2000, 4096, executable)).unwrap();
        assert_eq!(published(&project()), u32::from(writable.bits()));

        memory.map(request(0x3000, 0, executable)).unwrap();
        assert_eq!(
            published(&project()),
            u32::from(writable.union(Protection::EXECUTE).bits())
        );
    }

    #[test]
    fn projection_permissions_narrow_data_authority_but_retain_execute_identity() {
        let read_execute = Protection::READ.union(Protection::EXECUTE);
        let read_write_execute = read_execute.union(Protection::WRITE);
        assert_eq!(projection_permissions(Protection::READ, read_execute), read_execute);
        assert_eq!(
            projection_permissions(Protection::READ.union(Protection::WRITE), read_write_execute),
            read_write_execute,
        );
        assert_eq!(
            projection_permissions(Protection::READ, Protection::READ.union(Protection::WRITE)),
            Protection::READ,
        );
    }

    #[test]
    fn direct_literal_requires_complete_static_read_interval() {
        let ldr_x0 = u32::to_le_bytes(0x58000800);
        let source = [BorrowedSource {
            guest_first: 0x2000,
            bytes: &ldr_x0,
        }];
        assert!(direct_literal_target(0x2000, &source, 0x2100, 0x2108));
        assert!(!direct_literal_target(0x2000, &source, 0x2100, 0x2107));
        assert!(!direct_literal_target(0x2000, &source, 0x2101, 0x2200));
        let register_load = u32::to_le_bytes(0xf940_0000);
        assert!(!direct_literal_target(
            0x2000,
            &[BorrowedSource {
                guest_first: 0x2000,
                bytes: &register_load
            }],
            0,
            u64::MAX,
        ));
        let underflow = u32::to_le_bytes(0x58ff_ffe0);
        assert!(!direct_literal_target(
            0,
            &[BorrowedSource {
                guest_first: 0,
                bytes: &underflow
            }],
            0,
            u64::MAX,
        ));
    }

    #[test]
    fn scalar_access_forms() {
        for word in [0xf940_0020, 0xf840_0020, 0xf862_6820] {
            assert_eq!(InstructionWord(word).scalar_access(), Some(Protection::READ));
        }
        for word in [
            0xf840_8420, // post-index writeback
            0x3dc0_0020, // vector load
            0xf980_0020, // prefetch
        ] {
            assert_eq!(InstructionWord(word).scalar_access(), None);
        }
        assert_eq!(InstructionWord(0xf900_0020).scalar_access(), Some(Protection::WRITE));
    }

    #[test]
    fn vector_access_forms() {
        for word in [
            0x3dc0_0027, // ldr q7, [x1]
            0x3c81_0430, // str q16, [x1], #16
            0xad40_6bfb, // ldp q27, q26, [sp]
            0xbd40_0020, // ldr s0, [x1]
        ] {
            assert!(InstructionWord(word).vector_access(), "{word:#x}");
        }
        for word in [
            0xf940_0020, // ldr x0, [x1]
            0xa940_0020, // ldp x0, x0, [x1]
            0x4e28_4b70, // aese
            0x6e27_1e10, // eor v16.16b
            0x9100_4021, // add x1, x1, #16
            0x5400_0000, // b.eq
        ] {
            assert!(!InstructionWord(word).vector_access(), "{word:#x}");
        }
    }

    #[test]
    fn direct_scalar_trace_sees_a_write_past_a_branch_or_a_gap() {
        let executor = Executor::create().expect("executor");
        // The lock fast path: the authority must admit the load and the store
        // the run reaches after the branch, not just the later one.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xb940_0001_u32.to_le_bytes()); // ldr w1, [x0]
        bytes.extend_from_slice(&0x3500_0041_u32.to_le_bytes()); // cbnz w1, .+8
        bytes.extend_from_slice(&0xb900_0002_u32.to_le_bytes()); // str w2, [x0]
        assert_eq!(
            executor.direct_scalar_trace(
                0x1000,
                &[BorrowedSource {
                    guest_first: 0x1000,
                    bytes: &bytes,
                }]
            ),
            Some(Protection::READ.union(Protection::WRITE))
        );
        // A hole in the source spans hides no access behind it either.
        assert_eq!(
            executor.direct_scalar_trace(
                0x1000,
                &[
                    BorrowedSource {
                        guest_first: 0x1000,
                        bytes: &0xb940_0001_u32.to_le_bytes(),
                    },
                    BorrowedSource {
                        guest_first: 0x1008,
                        bytes: &0xb900_0002_u32.to_le_bytes(),
                    },
                ]
            ),
            Some(Protection::READ.union(Protection::WRITE))
        );
        // A vector access the run reaches after the branch still declines, since
        // direct authority has no operand it can certify for it.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xf940_03e0_u32.to_le_bytes()); // ldr x0, [sp]
        bytes.extend_from_slice(&0xd65f_03c0_u32.to_le_bytes()); // ret
        bytes.extend_from_slice(&0x3dc0_0027_u32.to_le_bytes()); // ldr q7, [x1]
        assert_eq!(
            executor.direct_scalar_trace(
                0x1000,
                &[BorrowedSource {
                    guest_first: 0x1000,
                    bytes: &bytes,
                }]
            ),
            None
        );
    }

    #[derive(Default)]
    struct FakeFaultOwner {
        events: std::sync::Mutex<Vec<&'static str>>,
        reject: bool,
        borrowed_leave: std::sync::atomic::AtomicU32,
        authority: std::sync::atomic::AtomicU64,
    }
    unsafe impl HostFaultOwner for FakeFaultOwner {
        unsafe fn publish(&self, view: HostFaultView) -> Result<(), ()> {
            assert!(!unsafe { view.contains(0) });
            // SAFETY: inside `publish`, the trait contract guarantees the scope's cpu
            // handle is live and lent to this thread until the matching unpublish.
            let cpu = unsafe { &*view.0.cpu };
            // SAFETY: the executor was created for aarch64, so the live cpu handle's
            // `state` points at an initialized `Aarch64Cpu` for the same interval.
            let state = unsafe { &*cpu.state.cast::<schema::Aarch64Cpu>() };
            self.authority
                .store(state.active_authority, std::sync::atomic::Ordering::Relaxed);
            let mut stolen = view.0;
            self.borrowed_leave.store(
                // SAFETY: `stolen` is a local copy of the still-published scope, so the
                // call is well-formed; the test asserts it is rejected as a non-owner.
                unsafe { hl_native_fault_scope_leave(&raw mut stolen) },
                std::sync::atomic::Ordering::Relaxed,
            );
            let mut events = self.events.lock().unwrap();
            assert!(events.last().is_none_or(|event| *event == "leave"));
            events.push("enter");
            (!self.reject).then_some(()).ok_or(())
        }
        unsafe fn unpublish(&self, _: HostFaultView) {
            let mut events = self.events.lock().unwrap();
            assert_eq!(events.last(), Some(&"enter"));
            events.push("leave");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn host_fault_owner_brackets_cold_and_warm_native_entries() {
        let words = [0xd4000001_u32];
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: words.as_ptr().cast(),
            size: 4,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let owner = std::sync::Arc::new(FakeFaultOwner::default());
        let executor = Executor::create_with_fault_owner(false, Some(owner.clone())).unwrap();
        executor.reset(1).unwrap();
        for _ in 0..2 {
            let mut cpu = Aarch64CpuState {
                pc: 0x4000,
                ..Aarch64CpuState::default()
            };
            assert_eq!(
                executor
                    .run_aarch64(&mut cpu, &source, None, 1, 1, None, None)
                    .unwrap()
                    .0,
                Exit::Syscall
            );
        }
        assert_eq!(*owner.events.lock().unwrap(), ["enter", "leave", "enter", "leave"]);
        assert_eq!(owner.borrowed_leave.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(owner.authority.load(std::sync::atomic::Ordering::Relaxed), 1);
        owner.authority.store(0, std::sync::atomic::Ordering::Relaxed);
        let stale_source = Source {
            spans: source.spans,
            span_count: source.span_count,
            mapping_incarnation: 2,
            instruction_epoch: 1,
        };
        let mut stale_cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        assert!(
            executor
                .run_aarch64(&mut stale_cpu, &stale_source, None, 1, 1, None, None)
                .is_err()
        );
        assert_eq!(owner.authority.load(std::sync::atomic::Ordering::Relaxed), 0);

        let branch_words = [0x14000001_u32, 0xd4000001_u32];
        let branch_span = SourceSpan {
            guest_first: 0x5000,
            bytes: branch_words.as_ptr().cast(),
            size: 8,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let branch_source = Source {
            spans: &raw const branch_span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        owner.events.lock().unwrap().clear();
        let mut cpu = Aarch64CpuState {
            pc: 0x5000,
            ..Aarch64CpuState::default()
        };
        assert_eq!(
            executor
                .run_aarch64(&mut cpu, &branch_source, None, 1, 2, None, None)
                .unwrap()
                .0,
            Exit::Syscall
        );
        assert_eq!(*owner.events.lock().unwrap(), ["enter", "leave", "enter", "leave"]);

        let rejecting = std::sync::Arc::new(FakeFaultOwner {
            events: std::sync::Mutex::new(Vec::new()),
            reject: true,
            borrowed_leave: std::sync::atomic::AtomicU32::new(u32::MAX),
            authority: std::sync::atomic::AtomicU64::new(0),
        });
        let rejected = Executor::create_with_fault_owner(false, Some(rejecting.clone())).unwrap();
        rejected.reset(1).unwrap();
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        assert!(rejected.run_aarch64(&mut cpu, &source, None, 1, 1, None, None).is_err());
        assert_eq!(*rejecting.events.lock().unwrap(), ["enter"]);
        rejected.reset(2).expect("publish rejection left no execution lease");
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    #[test]
    fn native_fault_owner_follows_worker_lifetime() {
        let owner = NativeFaultOwner::create().expect("process fault owner");
        owner.attach().expect("worker fault attachment");
        let words = [0xd4000001_u32];
        let span = SourceSpan {
            guest_first: 0x6000,
            bytes: words.as_ptr().cast(),
            size: 4,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        {
            let executor = Executor::create_with_fault_owner(false, Some(owner.clone())).unwrap();
            executor.reset(1).unwrap();
            let mut cpu = Aarch64CpuState {
                pc: 0x6000,
                ..Aarch64CpuState::default()
            };
            assert_eq!(
                executor
                    .run_aarch64(&mut cpu, &source, None, 1, 1, None, None)
                    .unwrap()
                    .0,
                Exit::Syscall,
            );
        }
        owner.detach();
    }

    #[test]
    fn view_hints_are_bounded_mru() {
        let mut hints = ViewHints::default();
        for address in [1, 2, 3, 4, 2] {
            hints.observe(address, Protection::READ);
        }
        assert_eq!(
            hints.entries,
            [(2, Protection::READ), (4, Protection::READ), (3, Protection::READ),]
        );
    }

    #[test]
    fn x86_dirty_journal_accepts_bounded_exact_ranges() {
        let mut native = NativeX86::capture(&X86CpuState::default(), false);
        native.0.memory_written = 1;
        native.0.dirty_view_first = 0x1000;
        native.0.dirty_view_last = 0x2000;
        native.0.dirty_first = 0x1100;
        native.0.dirty_last = 0x1108;
        let NativeWrites::Exact(ranges) = native.writes() else {
            panic!("not exact")
        };
        assert_eq!(ranges, [AddressRange::nonempty(GuestAddress::new(0x1100), 8).unwrap()]);
    }

    #[test]
    fn aarch64_dirty_journal_accepts_secondary_ranges() {
        let mut native = NativeAarch64::capture(&Aarch64CpuState::default());
        native.0.memory_written = 1;
        native.0.dirty_view_first = 0x3000;
        native.0.dirty_view_last = 0x4000;
        native.0.dirty_first = 0x3100;
        native.0.dirty_last = 0x3120;
        let NativeWrites::Exact(ranges) = native.writes() else {
            panic!("not exact")
        };
        assert_eq!(
            ranges,
            [AddressRange::nonempty(GuestAddress::new(0x3100), 0x20).unwrap()]
        );
    }

    #[test]
    fn aarch64_dirty_journal_preserves_overlap_and_overflow_fallback() {
        let mut native = NativeAarch64::capture(&Aarch64CpuState::default());
        assert!(matches!(native.writes(), NativeWrites::None));
        native.0.memory_written = 1;
        native.0.dirty_count = 2;
        native.0.dirty_records[0] = [0x1000, 0x2000, 0x1100, 0x1200];
        native.0.dirty_records[1] = [0x1000, 0x2000, 0x1180, 0x1300];
        let NativeWrites::Exact(ranges) = native.writes() else {
            panic!("not exact")
        };
        assert_eq!(ranges.len(), 2);
        native.0.dirty_overflow = 1;
        assert!(matches!(native.writes(), NativeWrites::Full));
    }

    #[test]
    fn x86_dirty_journal_falls_back_on_overflow_or_unknown_write() {
        let mut native = NativeX86::capture(&X86CpuState::default(), false);
        native.0.memory_written = 1;
        assert!(matches!(native.writes(), NativeWrites::Full));
        native.0.dirty_overflow = 1;
        assert!(matches!(native.writes(), NativeWrites::Full));
    }

    #[test]
    fn x86_dirty_journal_rejects_records_outside_their_view() {
        let mut native = NativeX86::capture(&X86CpuState::default(), false);
        native.0.memory_written = 1;
        native.0.dirty_count = 1;
        native.0.dirty_records[0] = [0x1000, 0x2000, 0x0ff8, 0x1000];
        assert!(matches!(native.writes(), NativeWrites::Full));
    }

    #[test]
    fn certificate_schema_is_dormant() {
        let aarch64 = NativeAarch64::capture(&Aarch64CpuState::default());
        let x86 = NativeX86::capture(&X86CpuState::default(), false);
        assert_eq!(aarch64.0.certificate_cache_identity, 0);
        assert_eq!(aarch64.0.certificate_token, 0);
        assert_eq!(x86.0.certificate_cache_identity, 0);
        assert_eq!(x86.0.certificate_token, 0);
        assert_eq!(
            std::mem::offset_of!(schema::Aarch64Cpu, certificate_cache_identity),
            2312
        );
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, certificate_token), 2320);
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, code_arena_lower), 2352);
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, code_arena_upper), 2360);
        assert_eq!(
            std::mem::offset_of!(schema::Aarch64Cpu, entry_certificate_identity),
            2368
        );
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, fault_completed), 2376);
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, ibtc_base), 2384);
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, execution_identity), 2392);
        assert_eq!(std::mem::offset_of!(schema::Aarch64Cpu, read_valid_count), 2400);
        assert_eq!(std::mem::size_of::<schema::Aarch64Cpu>(), 2408);
        assert_eq!(
            std::mem::offset_of!(schema::X86_64Cpu, certificate_cache_identity),
            1928
        );
        assert_eq!(std::mem::offset_of!(schema::X86_64Cpu, certificate_token), 1936);
        assert_eq!(std::mem::size_of::<schema::X86_64Cpu>(), 1944);
        assert_eq!(std::mem::size_of::<RunCertificate>(), 112);
        assert_eq!(std::mem::offset_of!(RunRequest, certificate), 160);
    }

    #[test]
    fn statistics_snapshots_follow_diagnostics_policy() {
        for (enabled, expected) in [(false, 0), (true, 2)] {
            let executor = Executor::create_diagnostics(enabled).expect("native executor");
            let _ = executor.statistics_snapshot().expect("before snapshot");
            let _ = executor.statistics_snapshot().expect("after snapshot");
            assert_eq!(
                executor.diagnostic_calls.load(std::sync::atomic::Ordering::Relaxed),
                expected,
            );
        }
    }

    #[test]
    fn interrupt_token_owns_atomic_native_storage() {
        let token = InterruptToken::create().expect("interrupt token");
        token.set(true).expect("set interrupt");
        token.set(false).expect("clear interrupt");
    }

    #[cfg(target_os = "linux")]
    fn mapping_permissions(address: *mut c_void) -> String {
        let address = address as usize;
        std::fs::read_to_string("/proc/self/maps")
            .expect("process mappings")
            .lines()
            .find_map(|line| {
                let (range, rest) = line.split_once(' ')?;
                let (first, last) = range.split_once('-')?;
                let first = usize::from_str_radix(first, 16).ok()?;
                let last = usize::from_str_radix(last, 16).ok()?;
                (first <= address && address < last)
                    .then(|| rest.split_whitespace().next().unwrap_or_default().to_owned())
            })
            .expect("mapping containing address")
    }

    #[test]
    fn aarch64_conversion_round_trips_architectural_state() {
        let mut cpu = Aarch64CpuState {
            registers: std::array::from_fn(|index| index as u64 * 17),
            vectors: std::array::from_fn(|index| (index as u128) << 72 | index as u128),
            sp: 0x1000,
            pc: 0x2000,
            nzcv: Nzcv::from_bits(0xb000_0000),
            tls: 0x3000,
            ..Aarch64CpuState::default()
        };
        let expected = cpu.clone();
        let native = NativeAarch64::capture(&cpu);
        assert_eq!(native.0.loop_valid, 0);
        assert_eq!(native.0.loop_view_count, 0);
        assert_eq!(native.0.loop_views, [[0; 6]; 2]);
        assert_eq!(native.0.loop_mapping_incarnation, 0);
        assert_eq!(native.0.loop_authority, 0);
        assert_eq!(native.0.loop_trip, 0);
        assert_eq!(native.0.loop_decrement, 0);
        assert_eq!(native.0.loop_instruction_count, 0);
        assert_eq!(native.0.loop_iterations, 0);
        assert_eq!(native.0.loop_budget_iterations, 0);
        assert_eq!(native.0.loop_executable, 0);
        assert_eq!(native.0.active_authority, 0);
        assert_eq!(native.0.active_view_incarnation, 0);
        assert_eq!(native.0.active_view_authority, 0);
        assert_eq!(native.0.code_arena_lower, 0);
        assert_eq!(native.0.code_arena_upper, 0);
        assert_eq!(native.0.entry_certificate_identity, 0);
        cpu = Aarch64CpuState::default();
        native.restore(&mut cpu);
        assert_eq!(cpu, expected);
    }

    #[test]
    fn x86_conversion_round_trips_native_state() {
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                registers: std::array::from_fn(|index| index as u64 * 19),
                flags: FlagState::from_bits(0x8d5),
                rip: 0x401000,
                fs_base: 0x7000,
                gs_base: 0x8000,
                direction: true,
                alignment_check: true,
                id_flag: true,
            },
            vectors: std::array::from_fn(|index| (index as u128) << 80 | index as u128),
            vector_upper: std::array::from_fn(|index| (index as u128) << 96 | !(index as u128)),
            mxcsr: 0xdfdf,
            ..X86CpuState::default()
        };
        let expected = cpu.clone();
        let native = NativeX86::capture(&cpu, false);
        cpu = X86CpuState::default();
        native.restore(&mut cpu);
        assert_eq!(cpu, expected);
    }

    #[test]
    fn x86_fp_environment_maps_control_and_status() {
        assert_eq!(NativeX86::fpcr(0x1f80) >> 22 & 3, 0);
        assert_eq!(NativeX86::fpcr(0x3f80) >> 22 & 3, 2);
        assert_eq!(NativeX86::fpcr(0x5f80) >> 22 & 3, 1);
        assert_eq!(NativeX86::fpcr(0x7f80) >> 22 & 3, 3);
        assert_eq!(NativeX86::fpcr(0x9f80) >> 24 & 1, 1);
        assert_eq!(NativeX86::fpcr(0x1fc0) >> 24 & 1, 1);
        assert_eq!(NativeX86::fpcr(0) & ((0x1f << 8) | (1 << 15)), 0);
        assert_eq!(NativeX86::exception_flags(NativeX86::fpsr(0x3f)), 0x3f);

        let mut cpu = X86CpuState {
            mxcsr: 0xdfc0,
            ..X86CpuState::default()
        };
        let mut native = NativeX86::capture(&cpu, false);
        native.0.fpsr |= NativeX86::fpsr(0x2d);
        native.restore(&mut cpu);
        assert_eq!(cpu.mxcsr & !0x3f, 0xdfc0 & !0x3f);
        assert_eq!(cpu.mxcsr & 0x3f, 0x2d);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_entry_restores_host_fp_environment() {
        fn host_environment() -> (u64, u64) {
            let fpcr: u64;
            let fpsr: u64;
            // SAFETY: gated to aarch64, these `mrs` reads copy the two FP control
            // registers into locals; they touch no memory and have no side effects.
            unsafe {
                std::arch::asm!("mrs {value}, fpcr", value = out(reg) fpcr);
                std::arch::asm!("mrs {value}, fpsr", value = out(reg) fpsr);
            }
            (fpcr, fpsr)
        }

        let before = host_environment();
        let executor = Executor::create().expect("native executor");
        let bytes = [0x90, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x9000,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x9000,
                ..Default::default()
            },
            mxcsr: 0xdfc0,
            ..X86CpuState::default()
        };
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 1, 2, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.mxcsr, 0xdfc0);
        assert_eq!(host_environment(), before);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_scalar_double_preserves_lanes_rounding_and_status() {
        let executor = Executor::create().expect("native executor");
        let bytes = [
            0xf2, 0x0f, 0x59, 0xc1, 0xf2, 0x0f, 0x58, 0xc1, 0xf2, 0x0f, 0x5e, 0xc1, 0xf2, 0x0f, 0x51, 0xc0, 0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0xa000,
            bytes: &bytes,
        }];
        let upper = 0xfeed_face_dead_beef_u128 << 64;
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0xa000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.vectors[0] = upper | u128::from(4_f64.to_bits());
        cpu.vectors[1] = u128::from(2_f64.to_bits());
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 1, 5, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.vectors[0] >> 64, upper >> 64);
        assert_eq!(cpu.vectors[0] as u64, 5_f64.sqrt().to_bits());
        assert_ne!(cpu.mxcsr & (1 << 5), 0);

        let divide = [0xf2, 0x0f, 0x5e, 0xc1, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0xa100,
            bytes: &divide,
        }];
        let mut up = X86CpuState {
            scalar: ScalarState {
                rip: 0xa100,
                ..Default::default()
            },
            mxcsr: 0x5f80,
            ..X86CpuState::default()
        };
        up.vectors[0] = u128::from(1_f64.to_bits());
        up.vectors[1] = u128::from(3_f64.to_bits());
        assert_eq!(
            executor
                .run_x86(&mut up, &source, 1, 2, 2, false, None, &mut resolve)
                .unwrap()
                .0
                .exit,
            Exit::Syscall
        );
        assert_eq!(up.vectors[0] as u64, (1_f64 / 3.0).to_bits() + 1);
        assert_ne!(up.mxcsr & (1 << 5), 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_scalar_double_nan_falls_back_before_commit() {
        let executor = Executor::create().expect("native executor");
        let bytes = [0xf2, 0x0f, 0x58, 0xc1, 0x0f, 0x0b];
        let source = [BorrowedSource {
            guest_first: 0xa200,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0xa200,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.vectors[0] = u128::from(f64::NAN.to_bits());
        cpu.vectors[1] = u128::from(1_f64.to_bits());
        let before = cpu.vectors;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 3, 1, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!((outcome.exit, outcome.instruction), (Exit::Fallback, 0xa200));
        assert_eq!(cpu.vectors, before);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_disabled_shadow_stack_read_preserves_destination() {
        let executor = Executor::create().expect("native executor");
        for (epoch, bytes) in [
            (1, &[0xf3, 0x0f, 0x1e, 0xc8, 0x0f, 0x05][..]),
            (2, &[0xf3, 0x48, 0x0f, 0x1e, 0xc8, 0x0f, 0x05][..]),
        ] {
            let source = [BorrowedSource {
                guest_first: 0xa300,
                bytes,
            }];
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0xa300,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[0] = 0xdead_beef_cafe_babe;
            let mut resolve = |_: u64, _: &mut [u8]| None;
            let outcome = executor
                .run_x86(&mut cpu, &source, 1, epoch, 2, false, None, &mut resolve)
                .unwrap()
                .0;
            assert_eq!(outcome.exit, Exit::Syscall);
            assert_eq!(cpu.registers[0], 0xdead_beef_cafe_babe);
        }
    }

    #[test]
    fn public_native_views_match_c_abi() {
        assert_eq!(std::mem::offset_of!(RunRequest, source), 32);
        assert_eq!(std::mem::offset_of!(RunRequest, source_context), 48);
        assert_eq!(std::mem::offset_of!(RunRequest, operand_context), 64);
        assert_eq!(std::mem::offset_of!(ProjectionView, permissions), 32);
        assert_eq!(std::mem::offset_of!(SourceSpan, instruction_epoch), 32);
    }

    #[test]
    fn executable_memory_is_w_x_and_released_once() {
        let mut memory = Box::new(ExecutableMemory::new());
        let context = std::ptr::from_mut(memory.as_mut()).cast();
        let mut mapping = Mapping {
            abi: 0,
            size: 0,
            handle: 0,
            writable: 0,
            executable: 0,
            capacity: 0,
            content: 0,
        };
        let dual = u32::from(cfg!(target_os = "linux"));
        assert_eq!(unsafe { reserve(context, 4096, 4096, dual, &raw mut mapping) }, 0);
        assert_eq!(unsafe { write_begin(context) }, 0);
        // SAFETY: `reserve` above succeeded with a 4096-byte mapping and `write_begin` made
        // it writable, so the first 16 bytes are in bounds and uniquely owned by this test.
        unsafe { std::ptr::write_bytes(memory.writable, 0xcc, 16) };
        assert_eq!(unsafe { publish(context, 1, 8, 16) }, 0);
        assert_eq!(unsafe { write_end(context) }, 0);
        assert_eq!(memory.published_ranges, 1);
        assert_eq!(memory.published_bytes, 16);
        if cfg!(target_os = "linux") {
            assert_ne!(memory.writable, memory.executable);
            assert_eq!(memory.write_transitions, 0);
            // SAFETY: 24 bytes lie inside the live 4096-byte readable executable alias, and
            // the borrow ends before any repair/release invalidates the mapping.
            let executable = unsafe { std::slice::from_raw_parts(memory.executable.cast::<u8>(), 24) };
            assert_eq!(&executable[..16], &[0xcc; 16]);
            #[cfg(target_os = "linux")]
            {
                let writable = mapping_permissions(memory.writable);
                let executable = mapping_permissions(memory.executable);
                assert!(writable.starts_with("rw-") && !writable.contains('x'));
                assert!(executable.starts_with("r-x") && !executable.contains('w'));
                let descriptor = memory.descriptor;
                mapping.content = 16;
                assert_eq!(unsafe { repair(context, &raw mut mapping, 1) }, 0);
                assert_ne!(memory.descriptor, descriptor);
                assert_eq!(mapping.writable, memory.writable as usize as u64);
                assert_eq!(mapping.executable, memory.executable as usize as u64);
                // SAFETY: the preserving repair returned 0, so `memory.executable` names the
                // new readable alias and 16 bytes are in bounds of its 4096-byte mapping.
                let executable = unsafe { std::slice::from_raw_parts(memory.executable.cast::<u8>(), 16) };
                assert_eq!(executable, &[0xcc; 16]);
                mapping.content = 0;
                assert_eq!(unsafe { repair(context, &raw mut mapping, 0) }, 0);
                // SAFETY: the discarding repair returned 0, so `memory.executable` names the
                // fresh readable alias and 16 bytes are in bounds of its 4096-byte mapping.
                let executable = unsafe { std::slice::from_raw_parts(memory.executable.cast::<u8>(), 16) };
                assert_eq!(executable, &[0; 16]);
            }
        } else {
            assert_eq!(memory.write_transitions, 2);
        }
        assert_eq!(unsafe { release(context, 1) }, 0);
        assert_eq!(unsafe { release(context, 1) }, 4);
        assert_eq!(memory.releases, 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fork_repair_privatises_dual_alias_backing() {
        let mut memory = Box::new(ExecutableMemory::new());
        let context = std::ptr::from_mut(memory.as_mut()).cast();
        let mut mapping = Mapping {
            abi: ABI,
            size: std::mem::size_of::<Mapping>() as u32,
            handle: 0,
            writable: 0,
            executable: 0,
            capacity: 0,
            content: 16,
        };
        assert_eq!(unsafe { reserve(context, 4096, 4096, 1, &raw mut mapping) }, 0);
        // SAFETY: `reserve` succeeded above with a 4096-byte writable mapping owned solely
        // by this test, so the first 16 bytes are in bounds.
        unsafe { std::ptr::write_bytes(memory.writable, 0x11, 16) };
        // SAFETY: fork takes no arguments; the child below touches only the async-signal
        // safe subset, so no lock held by another test thread can be observed.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            // SAFETY: `context` still addresses the inherited `Box<ExecutableMemory>` in
            // the child's copied address space, and repair only issues raw syscalls.
            let status = unsafe { repair(context, &raw mut mapping, 1) };
            if status == 0 {
                // SAFETY: repair succeeded, so `memory.writable` names the child's private
                // 4096-byte writable alias and 16 bytes are in bounds of it.
                unsafe { std::ptr::write_bytes(memory.writable, 0x22, 16) };
            }
            // SAFETY: `_exit` bypasses atfork handlers and destructors, which is required
            // because this child was forked from a multi-threaded test process.
            unsafe { libc::_exit(status as i32) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        // SAFETY: the parent's own mapping is untouched by the child and still live, so 16
        // bytes are in bounds; waitpid above ensures the child has already exited.
        let parent = unsafe { std::slice::from_raw_parts(memory.writable.cast::<u8>(), 16) };
        assert_eq!(parent, &[0x11; 16]);
        assert_eq!(unsafe { release(context, 1) }, 0);
    }

    #[test]
    fn executor_create_destroy_repeats_without_retained_mapping() {
        for _ in 0..8 {
            let executor = Executor::create().expect("native executor");
            assert!(!executor.memory.writable.is_null());
            drop(executor);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executor_emits_executes_and_resets() {
        let words = [0x01_u8, 0x00, 0x00, 0xd4];
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: words.as_ptr(),
            size: words.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(1).expect("initial epoch");
        for _ in 0..2 {
            let mut cpu = Aarch64CpuState {
                pc: 0x4000,
                ..Aarch64CpuState::default()
            };
            let (exit, instruction, _, _, remaining, executed, _) = executor
                .run_aarch64(&mut cpu, &source, None, 1, 1, None, None)
                .expect("native run");
            assert_eq!(exit, Exit::Syscall);
            assert_eq!(instruction, 0x4000);
            assert_eq!((remaining, executed), (0, 1));
        }
        let before_epoch = executor.diagnostics().unwrap();
        let changed_span = SourceSpan {
            guest_first: span.guest_first,
            bytes: span.bytes,
            size: span.size,
            mapping_incarnation: span.mapping_incarnation,
            instruction_epoch: 3,
        };
        let changed_source = Source {
            spans: &raw const changed_span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 3,
        };
        let mut changed_cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        executor.invalidate(0x4000, 0x4004, 1).expect("source invalidation");
        assert_eq!(
            executor
                .run_aarch64(&mut changed_cpu, &changed_source, None, 1, 1, None, None)
                .unwrap()
                .0,
            Exit::Syscall
        );
        let after_epoch = executor.diagnostics().unwrap();
        assert_eq!(after_epoch.cache_generation, before_epoch.cache_generation);
        assert!(after_epoch.invalidations > before_epoch.invalidations);
        assert_eq!(after_epoch.live_blocks, 1);
        executor.reset(1).expect("cache reset");
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        assert_eq!(
            executor
                .run_aarch64(&mut cpu, &source, None, 1, 1, None, None)
                .unwrap()
                .0,
            Exit::Syscall
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn aarch64_syscall_reentry_reloads_architectural_state() {
        let words = [
            0xd4000001_u32, // svc #0
            0x93407c00_u32, // sxtw x0,w0
            0xd4000001_u32, // svc #0
        ];
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: words.as_ptr().cast(),
            size: std::mem::size_of_val(&words),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(1).expect("initial epoch");
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            registers: std::array::from_fn(|index| match index {
                0 => 172,
                30 => 0x1234_5678_9abc_def0,
                _ => index as u64,
            }),
            nzcv: Nzcv::from_bits(0xa000_0000),
            ..Aarch64CpuState::default()
        };
        let first = executor.run_aarch64(&mut cpu, &source, None, 1, 3, None, None).unwrap();
        assert_eq!((first.0, first.1), (Exit::Syscall, 0x4000));
        assert_eq!(cpu.registers[0], 172);
        assert_eq!(cpu.registers[30], 0x1234_5678_9abc_def0);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);

        cpu.registers[0] = 0xaaaa_aaaa_8000_0001;
        cpu.pc = 0x4004;
        let second = executor.run_aarch64(&mut cpu, &source, None, 1, 2, None, None).unwrap();
        assert_eq!((second.0, second.1), (Exit::Syscall, 0x4008));
        assert_eq!(cpu.registers[0], 0xffff_ffff_8000_0001);
        assert_eq!(cpu.registers[30], 0x1234_5678_9abc_def0);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_executor_is_exact_and_reuses_cache() {
        let bytes = [0xb8, 1, 0, 0, 0, 0x01, 0xc0, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x4000,
            bytes: &bytes,
        }];
        let executor = Executor::create_diagnostics(true).expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut stopped = X86CpuState {
            scalar: ScalarState {
                rip: 0x4000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let zero = executor
            .run_x86(&mut stopped, &source, 1, 2, 0, false, None, &mut resolve)
            .unwrap();
        assert_eq!(zero.0.exit, Exit::Yield);
        assert_eq!((zero.0.remaining, zero.0.executed), (0, 0));
        assert_eq!(stopped.rip, 0x4000);
        let interrupted = executor
            .run_x86(&mut stopped, &source, 1, 2, 3, true, None, &mut resolve)
            .unwrap();
        assert_eq!(interrupted.0.exit, Exit::Interrupt);
        assert_eq!((interrupted.0.remaining, interrupted.0.executed), (3, 0));
        assert_eq!(stopped.rip, 0x4000);

        let mut first = X86CpuState {
            scalar: ScalarState {
                rip: 0x4000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let (result, cold, _) = executor
            .run_x86(&mut first, &source, 1, 2, 3, false, None, &mut resolve)
            .unwrap();
        assert_eq!(
            result,
            X86RunOutcome {
                exit: Exit::Syscall,
                instruction: 0x4007,
                next: 0x4009,
                address: 0,
                code: 0,
                remaining: 0,
                executed: 3,
            }
        );
        assert_eq!(first.registers[0], 2);
        let mut second = X86CpuState {
            scalar: ScalarState {
                rip: 0x4000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let (_, warm, _) = executor
            .run_x86(&mut second, &source, 1, 2, 3, false, None, &mut resolve)
            .unwrap();
        assert_eq!(warm.builds, 0);
        assert!(cold.builds == 1 && warm.hits >= 1);

        let start = std::time::Instant::now();
        for _ in 0..1000 {
            second.rip = 0x4000;
            second.registers[0] = 0;
            let (outcome, statistics, _) = executor
                .run_x86(&mut second, &source, 1, 2, 3, false, None, &mut resolve)
                .unwrap();
            assert_eq!(outcome.exit, Exit::Syscall);
            assert_eq!(statistics.builds, 0);
        }
        eprintln!("x86 native warm_runs=1000 elapsed={:?}", start.elapsed());
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_division_covers_widths_signs_and_fault_boundaries() {
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let forms: &[(&[u8], u64, u64, u64, u64)] = &[
            (&[0xf6, 0xf1, 0x0f, 0x05], 1_000, 0, 10, 100),
            (&[0x66, 0xf7, 0xf1, 0x0f, 0x05], 1_000, 0, 10, 100),
            (&[0xf7, 0xf1, 0x0f, 0x05], 1_000, 0, 10, 100),
            (&[0x48, 0xf7, 0xf1, 0x0f, 0x05], 1_000, 0, 10, 100),
        ];
        for (epoch, &(bytes, low, high, divisor, quotient)) in forms.iter().enumerate() {
            let source = [BorrowedSource {
                guest_first: 0x6000,
                bytes,
            }];
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x6000,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[0] = low;
            cpu.registers[2] = high;
            cpu.registers[1] = divisor;
            let (outcome, statistics, _) = executor
                .run_x86(&mut cpu, &source, 1, epoch as u64 + 10, 2, false, None, &mut resolve)
                .unwrap();
            assert_eq!(
                outcome.exit,
                Exit::Syscall,
                "epoch={epoch} outcome={outcome:?} stats={statistics:?}"
            );
            let width = match bytes[0] {
                0xf6 => 1,
                0x66 => 2,
                0xf7 => 4,
                _ => 8,
            };
            let mask = if width == 8 {
                u64::MAX
            } else {
                (1_u64 << (width * 8)) - 1
            };
            assert_eq!(cpu.registers[0] & mask, quotient);
            if width == 1 {
                assert_eq!((cpu.registers[0] >> 8) & 0xff, 0);
            } else {
                assert_eq!(cpu.registers[2] & mask, 0);
            }
        }

        let signed = [0x48, 0xf7, 0xf9, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x7000,
            bytes: &signed,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = (-1_000_i64) as u64;
        cpu.registers[2] = u64::MAX;
        cpu.registers[1] = 10;
        assert_eq!(
            executor
                .run_x86(&mut cpu, &source, 1, 20, 2, false, None, &mut resolve)
                .unwrap()
                .0
                .exit,
            Exit::Syscall
        );
        assert_eq!(cpu.registers[0] as i64, -100);
        assert_eq!(cpu.registers[2], 0);

        let signed_forms: &[(&[u8], u64, u64, u64)] = &[
            (&[0xf6, 0xf9, 0x0f, 0x05], (-1_000_i16) as u16 as u64, 0, 0xff9c),
            (&[0x66, 0xf7, 0xf9, 0x0f, 0x05], 0xfc18, 0xffff, 0xff9c),
            (&[0xf7, 0xf9, 0x0f, 0x05], 0xffff_fc18, 0xffff_ffff, 0xffff_ff9c),
        ];
        for (epoch, &(bytes, low, high, quotient)) in signed_forms.iter().enumerate() {
            let source = [BorrowedSource {
                guest_first: 0x7800,
                bytes,
            }];
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x7800,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[0] = low;
            cpu.registers[2] = high;
            cpu.registers[1] = 10;
            let outcome = executor
                .run_x86(&mut cpu, &source, 1, epoch as u64 + 21, 2, false, None, &mut resolve)
                .unwrap()
                .0;
            assert_eq!(outcome.exit, Exit::Syscall);
            let width = if bytes[0] == 0xf6 {
                1
            } else if bytes[0] == 0x66 {
                2
            } else {
                4
            };
            let mask = (1_u64 << (width * 8)) - 1;
            assert_eq!(cpu.registers[0] & mask, quotient & mask);
            if width == 1 {
                assert_eq!((cpu.registers[0] >> 8) & 0xff, 0);
            } else {
                assert_eq!(cpu.registers[2] & mask, 0);
            }
        }

        for (epoch, bytes, low, divisor) in [
            (30, [0xf6, 0xf1, 0x0f, 0x0b], 100_u64, 0_u64),
            (31, [0xf6, 0xf1, 0x0f, 0x0b], 0x1ff_u64, 1_u64),
        ] {
            let source = [BorrowedSource {
                guest_first: 0x8000,
                bytes: &bytes,
            }];
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x8000,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[0] = low;
            cpu.registers[1] = divisor;
            let before = cpu.clone();
            let outcome = executor
                .run_x86(&mut cpu, &source, 1, epoch, 1, false, None, &mut resolve)
                .unwrap()
                .0;
            assert_eq!((outcome.exit, outcome.instruction), (Exit::Fallback, 0x8000));
            assert_eq!(cpu.registers, before.registers);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_fallback_preserves_interpreter_boundary() {
        let bytes = [0xb8, 1, 0, 0, 0, 0x48, 0x8b, 0x03];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                registers: std::array::from_fn(|index| 0x100 + index as u64),
                flags: FlagState::from_bits(0x8d5),
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let (outcome, statistics, _) = executor
            .run_x86(&mut cpu, &source, 4, 5, 64, false, None, &mut resolve)
            .unwrap();
        assert_eq!(
            outcome,
            X86RunOutcome {
                exit: Exit::Fallback,
                instruction: 0x5005,
                next: 0x5005,
                address: 0,
                code: 0,
                remaining: 63,
                executed: 1,
            }
        );
        assert!(statistics.fallback);
        assert_eq!(cpu.rip, 0x5005);
        assert_eq!(cpu.registers[0], 1);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_fallback_preserves_prior_projected_write() {
        let bytes = [0x48, 0x89, 0x03, 0x48, 0x8b, 0x03];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut storage = [0_u8; 16];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 4,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 4,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 0x1122_3344_5566_7788;
        cpu.registers[3] = 0x7000;
        let (outcome, statistics, written) = executor
            .run_x86(&mut cpu, &source, 4, 6, 64, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(
            outcome,
            X86RunOutcome {
                exit: Exit::Fallback,
                instruction: 0x5006,
                next: 0x5006,
                address: 0,
                code: 0,
                remaining: 62,
                executed: 2,
            }
        );
        assert!(statistics.fallback && written);
        assert_eq!(cpu.rip, 0x5006);
        assert_eq!(&storage[..8], &0x1122_3344_5566_7788_u64.to_le_bytes());
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_repeated_byte_store_publishes_contiguous_range() {
        let bytes = [
            0x88, 0x06, // mov [rsi],al
            0x48, 0x83, 0xc6, 0x01, // add rsi,1
            0x83, 0xc0, 0x01, // add eax,1
            0x48, 0x39, 0xfe, // cmp rsi,rdi
            0x75, 0xf2, // jne 0x5000
            0x0f, 0x05, // syscall
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut storage = [0_u8; 4096];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x8000,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 4,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 4,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 1;
        cpu.registers[6] = 0x7000;
        cpu.registers[7] = 0x8000;
        let mut terminal = None;
        for _ in 0..32 {
            let (outcome, _, written) = executor
                .run_x86(&mut cpu, &source, 4, 6, 4096, false, Some(&projection), &mut resolve)
                .unwrap();
            assert!(written);
            let initialized = usize::try_from(cpu.registers[6] - 0x7000).unwrap();
            for (index, byte) in storage[..initialized].iter().copied().enumerate() {
                assert_eq!(byte, (index + 1) as u8, "boundary byte {index}");
            }
            if outcome.exit == Exit::Syscall {
                terminal = Some(outcome);
                break;
            }
        }
        assert_eq!(terminal.expect("syscall boundary").exit, Exit::Syscall);
        assert_eq!(cpu.registers[6], 0x8000);
        for (index, byte) in storage.into_iter().enumerate() {
            assert_eq!(byte, (index + 1) as u8, "byte {index}");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_stack_registers_and_immediates_are_transactional() {
        let bytes = [
            0x50, 0x41, 0x50, 0x6a, 0x80, /* push rax; push r8; push -128 */
            0x59, 0x41, 0x59, 0x5a, /* pop rcx; pop r9; pop rdx */
            0x66, 0x50, 0x66, 0x5b, /* push ax; pop bx */
            0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().expect("native executor");
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut storage = [0_u8; 64];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7040,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 4,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 4,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 0x1122_3344_5566_7788;
        cpu.registers[8] = 0x8877_6655_4433_2211;
        cpu.registers[3] = 0xaabb_ccdd_eeff_0000;
        cpu.registers[4] = 0x7040;
        let (outcome, _, written) = executor
            .run_x86(&mut cpu, &source, 4, 7, 64, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[4], 0x7040);
        assert_eq!(cpu.registers[1], u64::MAX - 127);
        assert_eq!(cpu.registers[9], 0x8877_6655_4433_2211);
        assert_eq!(cpu.registers[2], 0x1122_3344_5566_7788);
        assert_eq!(cpu.registers[3], 0xaabb_ccdd_eeff_7788);
        assert!(written);

        storage.fill(0xa5);
        let before = storage;
        let push = [0x50];
        let source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &push,
        }];
        cpu.rip = 0x6000;
        cpu.registers[4] = 0x7000;
        let (fault, _, written) = executor
            .run_x86(&mut cpu, &source, 4, 8, 1, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(fault.exit, Exit::Fallback);
        assert_eq!(cpu.registers[4], 0x7000);
        assert_eq!(storage, before);
        assert!(!written);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_projection_is_validated_before_execution() {
        let bytes = [0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &bytes,
        }];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x8000,
            host_first: 0x9000,
            mapping_incarnation: 8,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 8,
            active: 0,
        };
        let executor = Executor::create().unwrap();
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x6000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let expected = cpu.clone();
        let mut resolve = |_: u64, _: &mut [u8]| None;
        assert!(
            executor
                .run_x86(&mut cpu, &source, 7, 1, 1, false, Some(&projection), &mut resolve)
                .is_err()
        );
        assert_eq!(cpu, expected);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_entry_round_trips_all_vector_lanes() {
        let bytes = [0x90, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        for (index, vector) in cpu.vectors.iter_mut().enumerate() {
            *vector = (index as u128) << 96 | 0x8877_6655_4433_2211_00ff_eedd_ccbb_aa99;
        }
        for (index, vector) in cpu.vector_upper.iter_mut().enumerate() {
            *vector = (index as u128) << 96 | 0x99aa_bbcc_ddee_ff00_1122_3344_5566_7788;
        }
        let expected = (cpu.vectors, cpu.vector_upper);
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 1, 2, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!((cpu.vectors, cpu.vector_upper), expected);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_packed_bitwise() {
        let bytes = [
            0x66, 0x0f, 0xef, 0xc1, 0x66, 0x0f, 0xdb, 0xd3, 0x66, 0x0f, 0xeb, 0xe5, 0x66, 0x0f, 0xdf, 0xf7, 0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.vectors[..8].copy_from_slice(&[
            0x00ff_00ff_00ff_00ff_00ff_00ff_00ff_00ff,
            0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f,
            0xffff_0000_ffff_0000_ffff_0000_ffff_0000,
            0x3333_3333_3333_3333_3333_3333_3333_3333,
            0xaaaa_0000_aaaa_0000_aaaa_0000_aaaa_0000,
            0x0000_5555_0000_5555_0000_5555_0000_5555,
            0xffff_0000_ffff_0000_ffff_0000_ffff_0000,
            0x5555_5555_5555_5555_5555_5555_5555_5555,
        ]);
        let original = cpu.vectors;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 1, 5, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.vectors[0], original[0] ^ original[1]);
        assert_eq!(cpu.vectors[2], original[2] & original[3]);
        assert_eq!(cpu.vectors[4], original[4] | original[5]);
        assert_eq!(cpu.vectors[6], !original[6] & original[7]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_packed_memory() {
        let bytes = [0x66, 0x0f, 0xef, 0x03, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let operand = 0xaaaa_5555_0123_4567_89ab_cdef_fedc_ba98_u128;
        let mut storage = operand.to_le_bytes();
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 3,
            permissions: 1,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 3,
            active: 0,
        };
        let initial = 0x1111_2222_3333_4444_5555_6666_7777_8888_u128;
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[3] = 0x7000;
        cpu.vectors[0] = initial;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 3, 1, 2, false, Some(&projection), &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.vectors[0], initial ^ operand);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_set_conditions() {
        let bytes = [
            0x39, 0xc0, 0x0f, 0x90, 0xc0, 0x0f, 0x91, 0xc1, 0x0f, 0x92, 0xc2, 0x0f, 0x93, 0xc3, 0x40, 0x0f, 0x94, 0xc4,
            0x40, 0x0f, 0x95, 0xc5, 0x40, 0x0f, 0x96, 0xc6, 0x40, 0x0f, 0x97, 0xc7, 0x41, 0x0f, 0x98, 0xc0, 0x41, 0x0f,
            0x99, 0xc1, 0x41, 0x0f, 0x9a, 0xc2, 0x41, 0x0f, 0x9b, 0xc3, 0x41, 0x0f, 0x9c, 0xc4, 0x41, 0x0f, 0x9d, 0xc5,
            0x41, 0x0f, 0x9e, 0xc6, 0x41, 0x0f, 0x9f, 0xc7, 0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let baseline_bytes = [0x39, 0xc0, 0x0f, 0x05];
        let baseline_source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &baseline_bytes,
        }];
        let mut baseline = X86CpuState {
            scalar: ScalarState {
                rip: 0x6000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        let mut resolve = |_: u64, _: &mut [u8]| None;
        executor
            .run_x86(&mut baseline, &baseline_source, 1, 1, 2, false, None, &mut resolve)
            .unwrap();
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        for (index, register) in cpu.registers.iter_mut().enumerate() {
            *register = 0x1234_5678_9abc_de00 | index as u64;
        }
        let outcome = executor
            .run_x86(&mut cpu, &source, 1, 1, 18, false, None, &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.flags, baseline.flags);
        let expected = [0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0];
        for (index, expected) in expected.into_iter().enumerate() {
            assert_eq!(cpu.registers[index] & 0xff, expected, "condition {index:#x}");
            assert_eq!(cpu.registers[index] >> 8, 0x1234_5678_9abc_de, "register {index}");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_set_destinations() {
        let bytes = [
            0x39, 0xc0, 0x0f, 0x95, 0xc4, 0x0f, 0x94, 0x03, 0x0f, 0x95, 0x43, 0x01, 0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut storage = [0xaa_u8; 2];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7002,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 7,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 7,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 0x1122_3344_5566_7788;
        cpu.registers[3] = 0x7000;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 7, 1, 5, false, Some(&projection), &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[0], 0x1122_3344_5566_0088);
        assert_eq!(storage, [1, 0]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_movzx_forms() {
        let bytes = [
            0x0f, 0xb6, 0xc4, 0x40, 0x0f, 0xb6, 0xcc, 0x48, 0x0f, 0xb7, 0x13, 0x66, 0x0f, 0xb6, 0xf8, 0x0f, 0x05,
        ];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut storage = 0x1234_u16.to_le_bytes();
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7002,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 9,
            permissions: 1,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 9,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 0xffff_ffff_ffff_ab80;
        cpu.registers[3] = 0x7000;
        cpu.registers[4] = 0x8877_6655_4433_227d;
        cpu.registers[7] = 0x1122_3344_5566_7788;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let outcome = executor
            .run_x86(&mut cpu, &source, 9, 1, 5, false, Some(&projection), &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[0], 0xab);
        assert_eq!(cpu.registers[1], 0x7d);
        assert_eq!(cpu.registers[2], 0x1234);
        assert_eq!(cpu.registers[7], 0x1122_3344_5566_00ab);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_bitscan_forms() {
        let legacy_bytes = [0x0f, 0xbc, 0xc2, 0x0f, 0x94, 0xc1, 0x0f, 0x05];
        let legacy_source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &legacy_bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut legacy = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        legacy.registers[0] = 0x1122_3344_5566_7788;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        executor
            .run_x86(&mut legacy, &legacy_source, 1, 1, 3, false, None, &mut resolve)
            .unwrap();
        assert_eq!(legacy.registers[0], 0x5566_7788);
        assert_eq!(legacy.registers[1] & 0xff, 1);

        let count_bytes = [
            0xf3, 0x0f, 0xbc, 0xc2, 0x0f, 0x92, 0xc1, 0x0f, 0x94, 0xc3, 0x66, 0xf3, 0x0f, 0xbd, 0xc2, 0x0f, 0x05,
        ];
        let count_source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &count_bytes,
        }];
        let mut count = X86CpuState {
            scalar: ScalarState {
                rip: 0x6000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        count.registers[0] = 0xaaaa_bbbb_cccc_dddd;
        executor
            .run_x86(&mut count, &count_source, 1, 2, 5, false, None, &mut resolve)
            .unwrap();
        assert_eq!(count.registers[0] & 0xffff, 16);
        assert_eq!(count.registers[1] & 0xff, 1);
        assert_eq!(count.registers[3] & 0xff, 0);

        let memory_bytes = [0x48, 0x0f, 0xbd, 0x03, 0x0f, 0x05];
        let memory_source = [BorrowedSource {
            guest_first: 0x7000,
            bytes: &memory_bytes,
        }];
        let mut storage = 0x100_u64.to_le_bytes();
        let view = ProjectionView {
            guest_first: 0x8000,
            guest_last: 0x8008,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 4,
            permissions: 1,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 4,
            active: 0,
        };
        let mut memory = X86CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        memory.registers[3] = 0x8000;
        executor
            .run_x86(
                &mut memory,
                &memory_source,
                4,
                1,
                2,
                false,
                Some(&projection),
                &mut resolve,
            )
            .unwrap();
        assert_eq!(memory.registers[0], 8);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_data_strings_move_store_load() {
        let executor = Executor::create().unwrap();
        let mut storage = [0_u8; 64];
        storage[..16].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        let view = ProjectionView {
            guest_first: 0x8000,
            guest_last: 0x8040,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 12,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 12,
            active: 0,
        };
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let bytes = [0xf3, 0x48, 0xa5, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.flags = FlagState::from_bits(1 << 11);
        cpu.registers[1] = 2;
        cpu.registers[6] = 0x8000;
        cpu.registers[7] = 0x8030;
        let outcome = executor
            .run_x86(&mut cpu, &source, 12, 1, 100, false, Some(&projection), &mut resolve)
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(&storage[48..], &storage[..16]);
        assert_eq!(
            (cpu.registers[1], cpu.registers[6], cpu.registers[7]),
            (0, 0x8010, 0x8040)
        );

        for (encoding, width) in [
            (&[0xf3, 0xaa][..], 1usize),
            (&[0x66, 0xf3, 0xab][..], 2),
            (&[0xf3, 0xab][..], 4),
            (&[0xf3, 0x48, 0xab][..], 8),
        ] {
            storage[32..48].fill(0);
            let mut code = encoding.to_vec();
            code.extend_from_slice(&[0x0f, 0x05]);
            let source = [BorrowedSource {
                guest_first: 0x6000,
                bytes: &code,
            }];
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x6000,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[0] = 0x8877_6655_4433_2211;
            cpu.registers[1] = 2;
            cpu.registers[7] = 0x8020;
            executor
                .run_x86(
                    &mut cpu,
                    &source,
                    12,
                    2 + width as u64,
                    100,
                    false,
                    Some(&projection),
                    &mut resolve,
                )
                .unwrap();
            let pattern = cpu.registers[0].to_le_bytes();
            assert_eq!(&storage[32..32 + width], &pattern[..width]);
            assert_eq!(&storage[32 + width..32 + width * 2], &pattern[..width]);
            assert_eq!((cpu.registers[1], cpu.registers[7]), (0, 0x8020 + (width * 2) as u64));
        }

        let bytes = [0x48, 0xad, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x7000,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[6] = 0x8008;
        cpu.direction = true;
        executor
            .run_x86(&mut cpu, &source, 12, 20, 4, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(cpu.registers[0], 0x100f_0e0d_0c0b_0a09);
        assert_eq!(cpu.registers[6], 0x8000);

        let bytes = [0x67, 0xf3, 0xaa, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x7100,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x7100,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[1] = 1 << 32;
        cpu.registers[7] = 0xffff_ffff_0000_8000;
        let before = storage;
        executor
            .run_x86(&mut cpu, &source, 12, 21, 4, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(storage, before);
        assert_eq!((cpu.registers[1], cpu.registers[7]), (1 << 32, 0xffff_ffff_0000_8000));
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_large_rep_resumes_by_element_budget() {
        const COUNT: usize = (1 << 20) + 17;
        const BUDGET: u64 = 65_536;
        const FIRST: u64 = 0x20_0000;
        let executor = Executor::create().unwrap();
        let mut storage = vec![0_u8; COUNT * 2];
        for (index, byte) in storage[..COUNT].iter_mut().enumerate() {
            *byte = index as u8;
        }
        let view = ProjectionView {
            guest_first: FIRST,
            guest_last: FIRST + storage.len() as u64,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 31,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 31,
            active: 0,
        };
        let bytes = [0xf3, 0xa4, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[1] = COUNT as u64;
        cpu.registers[6] = FIRST;
        cpu.registers[7] = FIRST + COUNT as u64;
        let mut resolve = |_: u64, _: &mut [u8]| None;

        let mut polls = Vec::new();
        let mut poll = |executed, admitted| {
            polls.push((executed, admitted));
            true
        };
        let outcome = executor
            .run_x86_test(
                &mut cpu,
                &source,
                31,
                1,
                BUDGET,
                false,
                Some(&projection),
                &mut resolve,
                Some(&mut poll),
            )
            .unwrap()
            .0;
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(outcome.executed, COUNT as u64 + 1);
        assert_eq!(polls.len(), COUNT / BUDGET as usize);
        for (index, &(executed, admitted)) in polls.iter().enumerate() {
            let cumulative = (index as u64 + 1) * BUDGET;
            assert_eq!((executed, admitted), (cumulative, cumulative));
        }
        assert_eq!(
            (cpu.registers[1], cpu.registers[6], cpu.registers[7]),
            (0, FIRST + COUNT as u64, FIRST + (COUNT * 2) as u64)
        );
        assert_eq!(&storage[..COUNT], &storage[COUNT..]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_budget_return_child() {
        if std::env::var_os("HL_X86_BUDGET_RETURN_CHILD").is_none() {
            return;
        }
        let executor = Executor::create().unwrap();
        // Four NOPs; cmp eax,0; je to the first NOP. This is the same bounded
        // branch shape exercised by the native C budget contract.
        let bytes = [0x90, 0x90, 0x90, 0x90, 0x83, 0xf8, 0x00, 0x74, 0xf7];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let mut resolve = |_: u64, _: &mut [u8]| None;
        for budget in [1, 65_536] {
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x5000,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            let outcome = executor
                .run_x86(&mut cpu, &source, 41, 1, budget, false, None, &mut resolve)
                .unwrap()
                .0;
            if budget == 1 {
                assert_eq!(
                    (outcome.exit, outcome.executed, outcome.remaining, cpu.rip),
                    (Exit::Yield, 0, 1, 0x5000),
                    "a request smaller than the atomic block reports precise non-progress",
                );
            } else {
                assert_eq!(outcome.exit, Exit::Yield);
                assert!(outcome.executed > 0);
                assert_eq!(outcome.executed + outcome.remaining, budget);
                assert!(matches!(cpu.rip, 0x5000 | 0x5004));
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_native_small_and_large_budget_returns_are_bounded() {
        let executable = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(executable)
            .args(["x86_budget_return_child", "--nocapture"])
            .env("HL_X86_BUDGET_RETURN_CHILD", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        loop {
            match child.try_wait().unwrap() {
                Some(status) => {
                    assert!(status.success(), "native budget child failed: {status}");
                    break;
                }
                None if started.elapsed() < std::time::Duration::from_secs(3) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                None => {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("native budget child timed out");
                }
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_register_memory_alu_is_guarded() {
        let bytes = [0x4c, 0x01, 0x23, 0x48, 0x03, 0x03, 0x48, 0x3b, 0x0b, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut storage = [0_u8; 16];
        storage[..8].copy_from_slice(&10_u64.to_le_bytes());
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 11,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 11,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 2;
        cpu.registers[1] = 17;
        cpu.registers[3] = 0x7000;
        cpu.registers[12] = 5;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let (outcome, _, written) = executor
            .run_x86(&mut cpu, &source, 11, 1, 8, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(u64::from_le_bytes(storage[..8].try_into().unwrap()), 15);
        assert_eq!(cpu.registers[0], 17);
        assert!(written);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_segment_bases_are_per_run_and_guarded() {
        let load = [0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &load,
        }];
        let executor = Executor::create().unwrap();
        let mut storage = [0_u8; 32];
        storage[..8].copy_from_slice(&0x1122_3344_5566_7788_u64.to_le_bytes());
        storage[8..16].copy_from_slice(&0x8877_6655_4433_2211_u64.to_le_bytes());
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7020,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 10,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 10,
            active: 0,
        };
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut first = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                fs_base: 0x7000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        executor
            .run_x86(&mut first, &source, 10, 1, 4, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(first.registers[0], 0x1122_3344_5566_7788);

        let mut second = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                fs_base: 0x7008,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        executor
            .run_x86(&mut second, &source, 10, 1, 4, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(second.registers[0], 0x8877_6655_4433_2211);
        assert_eq!(first.fs_base, 0x7000);

        let store = [0x65, 0x48, 0x89, 0x04, 0x25, 0, 0, 0, 0, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &store,
        }];
        let before = storage;
        second.rip = 0x6000;
        second.gs_base = 0x7020;
        let (fault, _, written) = executor
            .run_x86(&mut second, &source, 10, 2, 2, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(fault.exit, Exit::Fallback);
        assert_eq!(storage, before);
        assert!(!written);
        assert_eq!(second.gs_base, 0x7020);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_immediate_memory_stores_are_guarded() {
        let bytes = [0x48, 0xc7, 0x44, 0x24, 0x08, 0x80, 0xff, 0xff, 0xff, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create().unwrap();
        let mut storage = [0_u8; 32];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7020,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 9,
            permissions: 3,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 9,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[4] = 0x7000;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let (outcome, _, written) = executor
            .run_x86(&mut cpu, &source, 9, 1, 8, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(&storage[8..16], &(u64::MAX - 127).to_le_bytes());
        assert!(written);

        storage.fill(0xa5);
        cpu.rip = 0x5000;
        cpu.registers[4] = 0x7019;
        let (fault, _, written) = executor
            .run_x86(&mut cpu, &source, 9, 2, 2, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(fault.exit, Exit::Fallback);
        assert_eq!(storage, [0xa5; 32]);
        assert!(!written);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_executable_store_forces_epoch_at_block_boundary() {
        let bytes = [0x48, 0xc7, 0x04, 0x24, 0x2a, 0, 0, 0, 0x0f, 0x05];
        let source = [BorrowedSource {
            guest_first: 0x5000,
            bytes: &bytes,
        }];
        let executor = Executor::create_diagnostics(true).unwrap();
        let mut storage = [0_u8; 16];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7010,
            host_first: storage.as_mut_ptr() as usize as u64,
            mapping_incarnation: 13,
            permissions: 7,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 13,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x5000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[4] = 0x7000;
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let before = executor.diagnostics().unwrap();
        let (outcome, _, written) = executor
            .run_x86(&mut cpu, &source, 13, 1, 2, false, Some(&projection), &mut resolve)
            .unwrap();
        let after = executor.diagnostics().unwrap();
        assert_eq!(outcome.exit, Exit::Epoch);
        assert_eq!(after.x86_public_epochs - before.x86_public_epochs, 1);
        assert_eq!(cpu.rip, 0x500a);
        assert_eq!(u64::from_le_bytes(storage[..8].try_into().unwrap()), 42);
        assert!(written);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_register_stores_use_checked_projection() {
        let cases: &[(&[u8], usize, u64, usize, &[u8])] = &[
            (&[0x88, 0x44, 0x24, 0x10, 0x0f, 0x05], 0, 0x1122, 1, &[0x22]),
            (&[0x88, 0x64, 0x24, 0x10, 0x0f, 0x05], 0, 0x1122, 1, &[0x11]),
            (&[0x66, 0x89, 0x44, 0x24, 0x10, 0x0f, 0x05], 0, 0x3344, 2, &[0x44, 0x33]),
            (
                &[0x89, 0x44, 0x24, 0x10, 0x0f, 0x05],
                0,
                0x5566_7788,
                4,
                &[0x88, 0x77, 0x66, 0x55],
            ),
            (
                &[0x4c, 0x89, 0x44, 0x24, 0x10, 0x0f, 0x05],
                8,
                0x1122_3344_5566_7788,
                8,
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
            ),
        ];
        let executor = Executor::create().unwrap();
        let mut resolve = |_: u64, _: &mut [u8]| None;
        for (epoch, &(bytes, register, value, width, expected)) in cases.iter().enumerate() {
            let mut storage = [0_u8; 64];
            let source = [BorrowedSource {
                guest_first: 0x6000,
                bytes,
            }];
            let view = ProjectionView {
                guest_first: 0x7000,
                guest_last: 0x7040,
                host_first: storage.as_mut_ptr() as usize as u64,
                mapping_incarnation: 1,
                permissions: 3,
                write_policy: WRITE_EXACT,
                write_index: 0,
            };
            let projection = Projection {
                views: &raw const view,
                count: 1,
                mapping_incarnation: 1,
                active: 0,
            };
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: 0x6000,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            cpu.registers[4] = 0x7000;
            cpu.registers[register] = value;
            let (outcome, _, written) = executor
                .run_x86(
                    &mut cpu,
                    &source,
                    1,
                    epoch as u64 + 10,
                    2,
                    false,
                    Some(&projection),
                    &mut resolve,
                )
                .unwrap();
            assert_eq!(outcome.exit, Exit::Syscall);
            assert!(written);
            assert_eq!(&storage[16..16 + width], expected);
        }
        let bytes = [0x48, 0x89, 0x44, 0x24, 0x10];
        let source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &bytes,
        }];
        let storage = [0_u8; 64];
        let view = ProjectionView {
            guest_first: 0x7000,
            guest_last: 0x7040,
            host_first: storage.as_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: 1,
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let mut cpu = X86CpuState {
            scalar: ScalarState {
                rip: 0x6000,
                ..Default::default()
            },
            ..X86CpuState::default()
        };
        cpu.registers[0] = 0xdead_beef;
        cpu.registers[4] = 0x7000;
        let expected = cpu.clone();
        let (outcome, _, written) = executor
            .run_x86(&mut cpu, &source, 1, 99, 1, false, Some(&projection), &mut resolve)
            .unwrap();
        assert_eq!(outcome.exit, Exit::Fallback);
        assert!(!written);
        assert_eq!(cpu, expected);
        assert_eq!(storage, [0; 64]);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn x86_calls_and_returns_commit_stack_transactionally() {
        let executor = Executor::create().unwrap();
        let mut resolve = |_: u64, _: &mut [u8]| None;
        let mut run = |entry: u64, bytes: &[(u64, &[u8])], registers: [u64; 16], storage: &mut [u8; 64], epoch| {
            let sources: Vec<_> = bytes
                .iter()
                .map(|&(guest_first, bytes)| BorrowedSource { guest_first, bytes })
                .collect();
            let view = ProjectionView {
                guest_first: 0x7000,
                guest_last: 0x7040,
                host_first: storage.as_mut_ptr() as usize as u64,
                mapping_incarnation: 1,
                permissions: 3,
                write_policy: WRITE_EXACT,
                write_index: 0,
            };
            let projection = Projection {
                views: &raw const view,
                count: 1,
                mapping_incarnation: 1,
                active: 0,
            };
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: entry,
                    registers,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            let outcome = executor
                .run_x86(&mut cpu, &sources, 1, epoch, 4, false, Some(&projection), &mut resolve)
                .unwrap_or_else(|()| panic!("native call failed at {entry:#x}"))
                .0;
            (cpu, outcome)
        };

        let direct = [0xe8, 2, 0, 0, 0, 0x0f, 0x05, 0xc3];
        let mut storage = [0; 64];
        let mut registers = [0; 16];
        registers[4] = 0x7030;
        let (cpu, outcome) = run(0x6000, &[(0x6000, &direct)], registers, &mut storage, 201);
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[4], 0x7030);
        assert_eq!(u64::from_le_bytes(storage[40..48].try_into().unwrap()), 0x6005);

        storage = [0; 64];
        storage[..8].copy_from_slice(&0x6200_u64.to_le_bytes());
        registers = [0; 16];
        registers[4] = 0x7000;
        let ret = [0xc2, 0x10, 0x00];
        let syscall = [0x0f, 0x05];
        let (cpu, outcome) = run(
            0x6100,
            &[(0x6100, &ret), (0x6200, &syscall)],
            registers,
            &mut storage,
            202,
        );
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[4], 0x7018);

        let call_register = [0xff, 0xd0, 0x0f, 0x05];
        let return_code = [0xc3];
        storage = [0; 64];
        registers = [0; 16];
        registers[0] = 0x6400;
        registers[4] = 0x7030;
        let (cpu, outcome) = run(
            0x6300,
            &[(0x6300, &call_register), (0x6400, &return_code)],
            registers,
            &mut storage,
            203,
        );
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[4], 0x7030);

        let call_memory = [0xff, 0x13, 0x0f, 0x05];
        storage = [0; 64];
        storage[..8].copy_from_slice(&0x6400_u64.to_le_bytes());
        registers = [0; 16];
        registers[3] = 0x7000;
        registers[4] = 0x7030;
        let (cpu, outcome) = run(
            0x6500,
            &[(0x6500, &call_memory), (0x6400, &return_code)],
            registers,
            &mut storage,
            204,
        );
        assert_eq!(outcome.exit, Exit::Syscall);
        assert_eq!(cpu.registers[4], 0x7030);

        let source = [BorrowedSource {
            guest_first: 0x6000,
            bytes: &direct,
        }];
        storage = [0; 64];
        registers = [0; 16];
        registers[4] = 0x7030;
        for (epoch, permissions, entry, bytes) in [
            (205, 1, 0x6000, source.as_slice()),
            (
                206,
                2,
                0x6100,
                [BorrowedSource {
                    guest_first: 0x6100,
                    bytes: &ret,
                }]
                .as_slice(),
            ),
        ] {
            let view = ProjectionView {
                guest_first: 0x7000,
                guest_last: 0x7040,
                host_first: storage.as_mut_ptr() as usize as u64,
                mapping_incarnation: 1,
                permissions,
                write_policy: WRITE_EXACT,
                write_index: 3,
            };
            let projection = Projection {
                views: &raw const view,
                count: 1,
                mapping_incarnation: 1,
                active: 0,
            };
            let before = storage;
            let mut cpu = X86CpuState {
                scalar: ScalarState {
                    rip: entry,
                    registers,
                    ..Default::default()
                },
                ..X86CpuState::default()
            };
            let outcome = executor
                .run_x86(&mut cpu, bytes, 1, epoch, 1, false, Some(&projection), &mut resolve)
                .unwrap()
                .0;
            assert_eq!(outcome.exit, Exit::Fallback);
            assert_eq!(cpu.registers[4], 0x7030);
            assert_eq!(storage, before);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn aarch64_branch_diagnostics_classify_source_exhaustion_without_changing_exit() {
        let words = [0xd503201f_u32; 32];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x3000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create_diagnostics(true).expect("diagnostic executor");
        executor.reset(1).expect("initial epoch");
        let mut cpu = Aarch64CpuState {
            pc: 0x3000,
            ..Aarch64CpuState::default()
        };
        let outcome = executor
            .run_aarch64(&mut cpu, &source, None, 1, 33, None, None)
            .expect("bounded exhaustion");
        assert_eq!(outcome.0, Exit::Fallback);
        assert_eq!((outcome.4, outcome.5, cpu.pc), (1, 32, 0x3080));
        let diagnostics = executor.diagnostics().expect("diagnostics");
        assert_eq!(diagnostics.a64_branch_exhaustion, 1);
        assert_eq!(diagnostics.a64_branch_cold_relocation, 0);
        assert_eq!(diagnostics.a64_branch_nonrelocatable, 0);
        assert_eq!(diagnostics.a64_branch_unidentified, 0);
        assert_eq!(
            (
                diagnostics.a64_branch_sample_pc,
                diagnostics.a64_branch_sample_source_first,
                diagnostics.a64_branch_sample_source_last,
                diagnostics.a64_branch_sample_form,
            ),
            (0x3080, 0x3000, 0x3080, A64_BRANCH_FORM_EXHAUSTION,),
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executor_retains_admission_across_ten_million_branches() {
        let words = [
            0xf1000400_u32, // subs x0,x0,#1
            0x54ffffe1,     // b.ne 0x4000
            0xd4000001,     // svc
        ];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(1).expect("initial epoch");
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            registers: std::array::from_fn(|index| u64::from(index == 0) * 10_000_000),
            ..Aarch64CpuState::default()
        };
        let outcome = executor
            .run_aarch64(&mut cpu, &source, None, 1, 20_000_001, None, None)
            .expect("ten-million-branch run");
        assert_eq!(outcome.0, Exit::Syscall);
        assert_eq!((outcome.4, outcome.5), (0, 20_000_001));
        assert_eq!(cpu.registers[0], 0);
        assert_eq!(cpu.pc, 0x4008);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executor_caches_ten_million_indirect_branches() {
        let words = [
            0xf1000400_u32, // subs x0,x0,#1
            0x54000040,     // b.eq 0x400c
            0xd61f0020,     // br x1
            0xd4000001,     // svc
        ];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create_diagnostics(true).expect("native executor");
        executor.reset(1).expect("initial epoch");
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            registers: std::array::from_fn(|index| match index {
                0 => 10_000_000,
                1 => 0x4000,
                _ => 0,
            }),
            ..Aarch64CpuState::default()
        };
        let outcome = executor
            .run_aarch64(&mut cpu, &source, None, 1, 30_000_000, None, None)
            .expect("ten-million-indirect run");
        assert_eq!(outcome.0, Exit::Syscall);
        assert_eq!((outcome.4, outcome.5), (0, 30_000_000));
        assert_eq!(cpu.registers[0], 0);
        assert_eq!(cpu.pc, 0x400c);
        let diagnostics = executor.diagnostics().expect("indirect diagnostics");
        assert_eq!(diagnostics.ibtc_authenticated_entries, 0);
        assert_eq!(diagnostics.ibtc_shared_hits, 0);
        assert_eq!(diagnostics.ibtc_auth_rejections, 0);
        executor
            .invalidate(0x4000, 0x4004, 1)
            .expect("invalidate patched source");
        cpu.pc = 0x4000;
        cpu.registers[0] = 10;
        assert_eq!(
            executor
                .run_aarch64(&mut cpu, &source, None, 1, 31, None, None)
                .unwrap()
                .0,
            Exit::Syscall,
        );
        assert_eq!(cpu.registers[0], 0);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executor_indirect_site_survives_polymorphism_and_hash_collision() {
        let branch = 0xd61f0020_u32; // br x1
        let service = 0xd4000001_u32; // svc
        let spans = [
            SourceSpan {
                guest_first: 0x4000,
                bytes: (&raw const branch).cast(),
                size: 4,
                mapping_incarnation: 1,
                instruction_epoch: 2,
            },
            SourceSpan {
                guest_first: 0x5000,
                bytes: (&raw const service).cast(),
                size: 4,
                mapping_incarnation: 1,
                instruction_epoch: 2,
            },
            /* (target >> 2) & 0xffff is identical to 0x5000. */
            SourceSpan {
                guest_first: 0x45000,
                bytes: (&raw const service).cast(),
                size: 4,
                mapping_incarnation: 1,
                instruction_epoch: 2,
            },
        ];
        let source = Source {
            spans: spans.as_ptr(),
            span_count: spans.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let executor = Executor::create_diagnostics(true).expect("native executor");
        executor.reset(1).expect("initial epoch");
        for target in [0x5000, 0x5000, 0x45000, 0x5000, 0x45000] {
            let mut cpu = Aarch64CpuState {
                pc: 0x4000,
                registers: std::array::from_fn(|index| if index == 1 { target } else { 0 }),
                ..Aarch64CpuState::default()
            };
            let outcome = executor.run_aarch64(&mut cpu, &source, None, 1, 2, None, None).unwrap();
            assert_eq!(outcome.0, Exit::Syscall, "target={target:#x}");
            assert_eq!(outcome.1, target, "target={target:#x}");
        }
        let diagnostics = executor.diagnostics().expect("collision diagnostics");
        assert_eq!(diagnostics.ibtc_authenticated_entries, 0);
        assert_eq!(diagnostics.ibtc_shared_hits, 0);
        assert_eq!(diagnostics.ibtc_auth_rejections, 0);
        executor
            .invalidate(0x45000, 0x45004, 1)
            .expect("invalidate collided target");
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            registers: std::array::from_fn(|index| if index == 1 { 0x45000 } else { 0 }),
            ..Aarch64CpuState::default()
        };
        assert_eq!(
            executor
                .run_aarch64(&mut cpu, &source, None, 1, 2, None, None)
                .unwrap()
                .0,
            Exit::Syscall
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn executor_recursion_uses_warm_multi_instruction_traces() {
        let words = [
            0xd2800400_u32, // movz x0,#32
            0x94000002,     // bl 0x400c
            0xd4000001,     // svc
            0xa9bf7bfd,     // stp x29,x30,[sp,#-16]!
            0x910003fd,     // mov x29,sp
            0xb4000080,     // cbz x0,0x4024
            0xd1000400,     // sub x0,x0,#1
            0x97fffffc,     // bl 0x400c
            0x91000400,     // add x0,x0,#1
            0xa8c17bfd,     // ldp x29,x30,[sp],#16
            0xd65f03c0,     // ret
        ];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 2,
        };
        let mut stack = vec![0_u8; 64 << 10];
        let view = ProjectionView {
            guest_first: 0x1_0000,
            guest_last: 0x2_0000,
            host_first: stack.as_mut_ptr() as usize as u64,
            mapping_incarnation: 1,
            permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
            write_policy: WRITE_EXACT,
            write_index: 0,
        };
        let projection = Projection {
            views: &raw const view,
            count: 1,
            mapping_incarnation: 1,
            active: 0,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(1).expect("initial epoch");
        let mut stopped = Aarch64CpuState {
            pc: 0x4000,
            sp: 0x1fff0,
            ..Aarch64CpuState::default()
        };
        let zero = executor
            .run_aarch64(&mut stopped, &source, Some(&projection), 1, 0, None, None)
            .unwrap();
        assert_eq!(zero.0, Exit::Yield);
        assert_eq!((zero.4, zero.5), (0, 0));
        assert_eq!(stopped.pc, 0x4000);
        let cold = executor.diagnostics().unwrap();
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            sp: 0x1fff0,
            ..Aarch64CpuState::default()
        };
        assert_eq!(
            executor
                .run_aarch64(&mut cpu, &source, Some(&projection), 1, 512, None, None)
                .unwrap()
                .0,
            Exit::Syscall
        );
        assert_eq!(cpu.registers[0], 32);
        assert_eq!(cpu.sp, 0x1fff0);
        let built = executor.diagnostics().unwrap();
        assert!(built.publications > cold.publications);
        assert!(built.publications - cold.publications < words.len() as u64);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            cpu.pc = 0x4000;
            cpu.sp = 0x1fff0;
            assert_eq!(
                executor
                    .run_aarch64(&mut cpu, &source, Some(&projection), 1, 512, None, None)
                    .unwrap()
                    .0,
                Exit::Syscall
            );
        }
        let warm = executor.diagnostics().unwrap();
        assert_eq!(warm.publications, built.publications);
        assert!(warm.cache_hits > built.cache_hits);
        eprintln!(
            "native recursion: blocks={} warm_runs=100 elapsed={:?}",
            built.publications - cold.publications,
            start.elapsed()
        );
        cpu = Aarch64CpuState {
            pc: 0x4000,
            sp: 0x1fff0,
            ..Aarch64CpuState::default()
        };
        let one = executor
            .run_aarch64(&mut cpu, &source, Some(&projection), 1, 1, None, None)
            .unwrap();
        assert_eq!(one.0, Exit::Yield);
        assert_eq!((one.4, one.5), (0, 1));
        assert_eq!(cpu.pc, 0x4004);
        assert_eq!(cpu.registers[0], 32);
        assert_eq!(cpu.registers[30], 0);
        cpu = Aarch64CpuState {
            pc: 0x4000,
            sp: 0x1fff0,
            ..Aarch64CpuState::default()
        };
        let two = executor
            .run_aarch64(&mut cpu, &source, Some(&projection), 1, 2, None, None)
            .unwrap();
        assert_eq!(two.0, Exit::Yield);
        assert_eq!((two.4, two.5), (0, 2));
        assert_eq!(cpu.pc, 0x400c);
        assert_eq!(cpu.registers[0], 32);
        assert_eq!(cpu.registers[30], 0x4008);
    }

    #[test]
    fn aarch64_projection_prefers_validated_active_writable_view() {
        let words = [
            0xf900_0020_u32, // str x0,[x1]
            0xf900_0820_u32, // str x0,[x1,#16]
            0xd400_0001,     // svc
        ];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 7,
            instruction_epoch: 3,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 7,
            instruction_epoch: 3,
        };
        let mut low = [0_u8; 16];
        let mut writable = [0_u8; 32];
        let views = [
            ProjectionView {
                guest_first: 0x1000,
                guest_last: 0x1010,
                host_first: low.as_mut_ptr() as usize as u64,
                mapping_incarnation: 7,
                permissions: u32::from(Protection::READ.bits()),
                write_policy: WRITE_EXACT,
                write_index: 0,
            },
            ProjectionView {
                guest_first: 0x8000,
                guest_last: 0x8020,
                host_first: writable.as_mut_ptr() as usize as u64,
                mapping_incarnation: 7,
                permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
                write_policy: WRITE_EXACT,
                write_index: 0,
            },
        ];
        let projection = Projection {
            views: views.as_ptr(),
            count: views.len(),
            mapping_incarnation: 7,
            active: 1,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(7).expect("mapping epoch");
        let value = 0x1122_3344_5566_7788_u64;
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        cpu.registers[0] = value;
        cpu.registers[1] = 0x8000;
        let outcome = executor
            .run_aarch64(&mut cpu, &source, Some(&projection), 7, 3, None, None)
            .unwrap();
        assert_eq!(outcome.0, Exit::Syscall);
        assert_eq!(u64::from_le_bytes(writable[..8].try_into().unwrap()), value);
        assert_eq!(low, [0; 16]);
        let NativeWrites::Exact(ranges) = outcome.6 else {
            panic!("expected exact dirty publication")
        };
        assert_eq!(u64::from_le_bytes(writable[16..24].try_into().unwrap()), value);
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start().get(), ranges[0].end().get()), (0x8000, 0x8008));
        assert_eq!((ranges[1].start().get(), ranges[1].end().get()), (0x8010, 0x8018));

        let invalid_active = Projection {
            active: 2,
            ..projection
        };
        assert!(
            executor
                .run_aarch64(&mut cpu, &source, Some(&invalid_active), 7, 1, None, None)
                .is_err()
        );
        let overlapping = [
            ProjectionView {
                guest_first: 0x1000,
                guest_last: 0x1010,
                host_first: low.as_mut_ptr() as usize as u64,
                mapping_incarnation: 7,
                permissions: u32::from(Protection::READ.bits()),
                write_policy: WRITE_EXACT,
                write_index: 0,
            },
            ProjectionView {
                guest_first: 0x1008,
                guest_last: 0x1018,
                host_first: writable.as_mut_ptr() as usize as u64,
                mapping_incarnation: 7,
                permissions: u32::from(Protection::READ.union(Protection::WRITE).bits()),
                write_policy: WRITE_EXACT,
                write_index: 0,
            },
        ];
        let invalid_overlap = Projection {
            views: overlapping.as_ptr(),
            active: 0,
            ..projection
        };
        assert!(
            executor
                .run_aarch64(&mut cpu, &source, Some(&invalid_overlap), 7, 1, None, None)
                .is_err()
        );
    }

    #[test]
    fn cached_write_views() {
        let words = [
            0xf900_0020_u32, // str x0,[x1]
            0xf900_0040_u32, // str x0,[x2]
            0xd400_0001,     // svc
        ];
        // SAFETY: `words` outlives `bytes`, and size_of_val is its exact byte length, so
        // the range is in bounds; u32 is plain data and u8 has weaker alignment.
        let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(&words)) };
        let span = SourceSpan {
            guest_first: 0x5000,
            bytes: bytes.as_ptr(),
            size: bytes.len(),
            mapping_incarnation: 8,
            instruction_epoch: 4,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 8,
            instruction_epoch: 4,
        };
        let mut low = [0_u8; 16];
        let mut high = [0_u8; 16];
        let permissions = u32::from(Protection::READ.union(Protection::WRITE).bits());
        let mut views = [
            ProjectionView {
                guest_first: 0x1000,
                guest_last: 0x1010,
                host_first: low.as_mut_ptr() as usize as u64,
                mapping_incarnation: 8,
                permissions,
                write_policy: WRITE_EXACT,
                write_index: 3,
            },
            ProjectionView {
                guest_first: 0x8000,
                guest_last: 0x8010,
                host_first: high.as_mut_ptr() as usize as u64,
                mapping_incarnation: 8,
                permissions,
                write_policy: WRITE_EXACT,
                write_index: 9,
            },
        ];
        let projection = Projection {
            views: views.as_ptr(),
            count: views.len(),
            mapping_incarnation: 8,
            active: 1,
        };
        let executor = Executor::create().expect("native executor");
        executor.reset(8).expect("mapping epoch");
        let value = 0xaabb_ccdd_1122_3344_u64;
        let mut cpu = Aarch64CpuState {
            pc: 0x5000,
            ..Aarch64CpuState::default()
        };
        cpu.registers[0] = value;
        cpu.registers[1] = 0x1000;
        cpu.registers[2] = 0x8000;
        let outcome = executor
            .run_aarch64(&mut cpu, &source, Some(&projection), 8, 3, None, None)
            .unwrap();
        assert_eq!(outcome.0, Exit::Syscall);
        assert_eq!(u64::from_le_bytes(low[..8].try_into().unwrap()), value);

        assert_eq!(u64::from_le_bytes(high[..8].try_into().unwrap()), value);
        let NativeWrites::Exact(ranges) = outcome.6 else {
            panic!("expected exact dirty publication")
        };
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start().get(), ranges[0].end().get()), (0x1000, 0x1008));
        assert_eq!((ranges[1].start().get(), ranges[1].end().get()), (0x8000, 0x8008));

        views[0].write_policy = 0;
        let stale_policy = Projection {
            views: views.as_ptr(),
            ..projection
        };
        assert!(
            executor
                .run_aarch64(&mut cpu, &source, Some(&stale_policy), 8, 1, None, None)
                .is_err()
        );
        views[0].write_policy = WRITE_EXACT;

        views[0].permissions = u32::from(Protection::READ.bits());
        let denied_projection = Projection {
            views: views.as_ptr(),
            ..projection
        };
        let mut denied = Aarch64CpuState {
            pc: 0x5000,
            ..Aarch64CpuState::default()
        };
        denied.registers[0] = 0xffff_ffff_ffff_ffff;
        denied.registers[1] = 0x1000;
        let denied_outcome = executor
            .run_aarch64(&mut denied, &source, Some(&denied_projection), 8, 1, None, None)
            .unwrap();
        assert_eq!(denied_outcome.0, Exit::Fallback);
        assert_eq!(u64::from_le_bytes(low[..8].try_into().unwrap()), value);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn asynchronous_token_interrupts_aarch64_direct_loop() {
        let executor = Executor::create().expect("native executor");
        executor.reset(1).expect("mapping epoch");
        let token = std::sync::Arc::new(InterruptToken::create().expect("interrupt token"));
        assert!(!token.is_set());
        let word = 0x1400_0000_u32; // b .
        let span = SourceSpan {
            guest_first: 0x4000,
            bytes: (&raw const word).cast(),
            size: std::mem::size_of_val(&word),
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let source = Source {
            spans: &raw const span,
            span_count: 1,
            mapping_incarnation: 1,
            instruction_epoch: 1,
        };
        let setter = {
            let token = token.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(2));
                token.set(true).unwrap();
            })
        };
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Aarch64CpuState::default()
        };
        let outcome = executor
            .run_aarch64_inner(
                &mut cpu,
                &source,
                None,
                1,
                u64::MAX / 2,
                None,
                None,
                Some(&token),
                None,
                None,
            )
            .expect("interruptible run");
        setter.join().unwrap();
        assert!(token.is_set());
        assert_eq!(outcome.0, Exit::Interrupt);
        assert_eq!(cpu.pc, 0x4000);
        token.set(false).unwrap();
        assert!(!token.is_set());
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
#[path = "executor_differential.rs"]
mod differential;
