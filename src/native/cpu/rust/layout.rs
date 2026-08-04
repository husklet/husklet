// Generated from ../layout.tsv by ../generate.rs.

#[repr(C)]
pub struct Aarch64Cpu {
    pub registers: [u64; 31],
    pub stack: u64,
    pub program: u64,
    pub tls: u64,
    pub reason: u64,
    pub host_stack: u64,
    pub host_registers: [u64; 12],
    pub vectors: [u64; 64],
    pub host_vectors: [u64; 16],
    pub flags: u64,
    pub indirect_site: u64,
    pub interrupt: u64,
    pub memory_first: u64,
    pub memory_last: u64,
    pub memory_delta: u64,
    pub memory_permissions: u64,
    pub fault_address: u64,
    pub fault_access: u64,
    pub fault_size: u64,
    pub budget: u64,
    pub executed: u64,
    pub memory_written: u64,
    pub dirty_view_first: u64,
    pub dirty_view_last: u64,
    pub dirty_first: u64,
    pub dirty_last: u64,
    pub dirty_count: u64,
    pub dirty_overflow: u64,
    pub dirty_records: [[u64; 4]; 16],
    pub read_token: u64,
    pub read_incarnation: u64,
    pub read_count: u64,
    pub read_views: [[u64; 4]; 4],
    pub interrupt_token: u64,
    pub executable_written: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub certificate_valid: u64,
    pub certificate_delta: u64,
    pub active_authority: u64,
    pub loop_valid: u64,
    pub loop_view_count: u64,
    pub loop_views: [[u64; 6]; 2],
    pub loop_mapping_incarnation: u64,
    pub loop_authority: u64,
    pub loop_trip: u64,
    pub loop_decrement: u64,
    pub loop_instruction_count: u64,
    pub loop_iterations: u64,
    pub loop_budget_iterations: u64,
    pub loop_executable: u64,
    pub active_view_incarnation: u64,
    pub active_view_authority: u64,
    pub diagnostic_guard_fast: u64,
    pub diagnostic_guard_full: u64,
    pub diagnostic_guard_fallback: u64,
    pub diagnostic_dirty_reserved: u64,
    pub diagnostic_dirty_overflow: u64,
    pub diagnostic_dirty_committed: u64,
    pub diagnostic_dirty_merged: u64,
}

#[repr(C)]
pub struct X86_64Cpu {
    pub registers: [u64; 16],
    pub program: u64,
    pub flags: u64,
    pub fs: u64,
    pub gs: u64,
    pub reason: u64,
    pub host_stack: u64,
    pub host_registers: [u64; 12],
    pub host_vectors: [u64; 16],
    pub vectors: [u64; 32],
    pub scratch: [u64; 2],
    pub interrupt: u64,
    pub indirect_site: u64,
    pub memory_first: u64,
    pub memory_last: u64,
    pub memory_delta: u64,
    pub memory_permissions: u64,
    pub fault_address: u64,
    pub fault_access: u64,
    pub fault_size: u64,
    pub memory_written: u64,
    pub budget: u64,
    pub executed: u64,
    pub loop_remaining: u64,
    pub loop_completed: u64,
    pub loop_block_count: u64,
    pub loop_pc: u64,
    pub dirty_view_first: u64,
    pub dirty_view_last: u64,
    pub dirty_first: u64,
    pub dirty_last: u64,
    pub dirty_count: u64,
    pub dirty_overflow: u64,
    pub dirty_records: [[u64; 4]; 16],
    pub read_token: u64,
    pub read_incarnation: u64,
    pub read_count: u64,
    pub read_views: [[u64; 4]; 4],
    pub executable_written: u64,
    pub mxcsr: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub host_fpcr: u64,
    pub host_fpsr: u64,
    pub vector_dirty: u64,
}

const _: () = {
    assert!(std::mem::offset_of!(Aarch64Cpu, registers) == 0);
    assert!(std::mem::offset_of!(Aarch64Cpu, stack) == 248);
    assert!(std::mem::offset_of!(Aarch64Cpu, program) == 256);
    assert!(std::mem::offset_of!(Aarch64Cpu, tls) == 264);
    assert!(std::mem::offset_of!(Aarch64Cpu, reason) == 272);
    assert!(std::mem::offset_of!(Aarch64Cpu, host_stack) == 280);
    assert!(std::mem::offset_of!(Aarch64Cpu, host_registers) == 288);
    assert!(std::mem::offset_of!(Aarch64Cpu, vectors) == 384);
    assert!(std::mem::offset_of!(Aarch64Cpu, host_vectors) == 896);
    assert!(std::mem::offset_of!(Aarch64Cpu, flags) == 1024);
    assert!(std::mem::offset_of!(Aarch64Cpu, indirect_site) == 1032);
    assert!(std::mem::offset_of!(Aarch64Cpu, interrupt) == 1040);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_first) == 1048);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_last) == 1056);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_delta) == 1064);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_permissions) == 1072);
    assert!(std::mem::offset_of!(Aarch64Cpu, fault_address) == 1080);
    assert!(std::mem::offset_of!(Aarch64Cpu, fault_access) == 1088);
    assert!(std::mem::offset_of!(Aarch64Cpu, fault_size) == 1096);
    assert!(std::mem::offset_of!(Aarch64Cpu, budget) == 1104);
    assert!(std::mem::offset_of!(Aarch64Cpu, executed) == 1112);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_written) == 1120);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_view_first) == 1128);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_view_last) == 1136);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_first) == 1144);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_last) == 1152);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_count) == 1160);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_overflow) == 1168);
    assert!(std::mem::offset_of!(Aarch64Cpu, dirty_records) == 1176);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_token) == 1688);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_incarnation) == 1696);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_count) == 1704);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_views) == 1712);
    assert!(std::mem::offset_of!(Aarch64Cpu, interrupt_token) == 1840);
    assert!(std::mem::offset_of!(Aarch64Cpu, executable_written) == 1848);
    assert!(std::mem::offset_of!(Aarch64Cpu, fpcr) == 1856);
    assert!(std::mem::offset_of!(Aarch64Cpu, fpsr) == 1864);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_valid) == 1872);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_delta) == 1880);
    assert!(std::mem::offset_of!(Aarch64Cpu, active_authority) == 1888);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_valid) == 1896);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_view_count) == 1904);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_views) == 1912);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_mapping_incarnation) == 2008);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_authority) == 2016);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_trip) == 2024);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_decrement) == 2032);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_instruction_count) == 2040);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_iterations) == 2048);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_budget_iterations) == 2056);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_executable) == 2064);
    assert!(std::mem::offset_of!(Aarch64Cpu, active_view_incarnation) == 2072);
    assert!(std::mem::offset_of!(Aarch64Cpu, active_view_authority) == 2080);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_fast) == 2088);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_full) == 2096);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_fallback) == 2104);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_reserved) == 2112);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_overflow) == 2120);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_committed) == 2128);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_merged) == 2136);
    assert!(std::mem::size_of::<Aarch64Cpu>() == 2144);

    assert!(std::mem::align_of::<Aarch64Cpu>() == 8);

    assert!(std::mem::offset_of!(X86_64Cpu, registers) == 0);
    assert!(std::mem::offset_of!(X86_64Cpu, program) == 128);
    assert!(std::mem::offset_of!(X86_64Cpu, flags) == 136);
    assert!(std::mem::offset_of!(X86_64Cpu, fs) == 144);
    assert!(std::mem::offset_of!(X86_64Cpu, gs) == 152);
    assert!(std::mem::offset_of!(X86_64Cpu, reason) == 160);
    assert!(std::mem::offset_of!(X86_64Cpu, host_stack) == 168);
    assert!(std::mem::offset_of!(X86_64Cpu, host_registers) == 176);
    assert!(std::mem::offset_of!(X86_64Cpu, host_vectors) == 272);
    assert!(std::mem::offset_of!(X86_64Cpu, vectors) == 400);
    assert!(std::mem::offset_of!(X86_64Cpu, scratch) == 656);
    assert!(std::mem::offset_of!(X86_64Cpu, interrupt) == 672);
    assert!(std::mem::offset_of!(X86_64Cpu, indirect_site) == 680);
    assert!(std::mem::offset_of!(X86_64Cpu, memory_first) == 688);
    assert!(std::mem::offset_of!(X86_64Cpu, memory_last) == 696);
    assert!(std::mem::offset_of!(X86_64Cpu, memory_delta) == 704);
    assert!(std::mem::offset_of!(X86_64Cpu, memory_permissions) == 712);
    assert!(std::mem::offset_of!(X86_64Cpu, fault_address) == 720);
    assert!(std::mem::offset_of!(X86_64Cpu, fault_access) == 728);
    assert!(std::mem::offset_of!(X86_64Cpu, fault_size) == 736);
    assert!(std::mem::offset_of!(X86_64Cpu, memory_written) == 744);
    assert!(std::mem::offset_of!(X86_64Cpu, budget) == 752);
    assert!(std::mem::offset_of!(X86_64Cpu, executed) == 760);
    assert!(std::mem::offset_of!(X86_64Cpu, loop_remaining) == 768);
    assert!(std::mem::offset_of!(X86_64Cpu, loop_completed) == 776);
    assert!(std::mem::offset_of!(X86_64Cpu, loop_block_count) == 784);
    assert!(std::mem::offset_of!(X86_64Cpu, loop_pc) == 792);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_view_first) == 800);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_view_last) == 808);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_first) == 816);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_last) == 824);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_count) == 832);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_overflow) == 840);
    assert!(std::mem::offset_of!(X86_64Cpu, dirty_records) == 848);
    assert!(std::mem::offset_of!(X86_64Cpu, read_token) == 1360);
    assert!(std::mem::offset_of!(X86_64Cpu, read_incarnation) == 1368);
    assert!(std::mem::offset_of!(X86_64Cpu, read_count) == 1376);
    assert!(std::mem::offset_of!(X86_64Cpu, read_views) == 1384);
    assert!(std::mem::offset_of!(X86_64Cpu, executable_written) == 1512);
    assert!(std::mem::offset_of!(X86_64Cpu, mxcsr) == 1520);
    assert!(std::mem::offset_of!(X86_64Cpu, fpcr) == 1528);
    assert!(std::mem::offset_of!(X86_64Cpu, fpsr) == 1536);
    assert!(std::mem::offset_of!(X86_64Cpu, host_fpcr) == 1544);
    assert!(std::mem::offset_of!(X86_64Cpu, host_fpsr) == 1552);
    assert!(std::mem::offset_of!(X86_64Cpu, vector_dirty) == 1560);
    assert!(std::mem::size_of::<X86_64Cpu>() == 1568);

    assert!(std::mem::align_of::<X86_64Cpu>() == 8);

};
