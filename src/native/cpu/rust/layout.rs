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
    pub read_view_publication: [[u64; 2]; 4],
    pub memory_write_policy: u64,
    pub memory_write_index: u64,
    pub certificate_guest_first: u64,
    pub certificate_guest_last: u64,
    pub certificate_host_first: u64,
    pub certificate_data_permissions: u64,
    pub certificate_mapped_executable: u64,
    pub certificate_mapping_incarnation: u64,
    pub certificate_mapping_generation: u64,
    pub certificate_instruction_generation: u64,
    pub certificate_authority_identity: u64,
    pub certificate_authority_generation: u64,
    pub certificate_run_generation: u64,
    pub certificate_view_index: u64,
    pub certificate_write_policy: u64,
    pub certificate_cache_identity: u64,
    pub certificate_token: u64,
    pub diagnostic_ibtc_authenticated_entries: u64,
    pub diagnostic_ibtc_shared_hits: u64,
    pub diagnostic_ibtc_auth_rejections: u64,
    pub code_arena_lower: u64,
    pub code_arena_upper: u64,
    pub entry_certificate_identity: u64,
    pub fault_completed: u64,
    pub ibtc_base: u64,
    pub execution_identity: u64,
    pub read_valid_count: u64,
    pub reserve_filter: [u64; 8],
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
    pub vector_upper: [u64; 32],
    pub certificate_guest_first: u64,
    pub certificate_guest_last: u64,
    pub certificate_host_first: u64,
    pub certificate_data_permissions: u64,
    pub certificate_mapped_executable: u64,
    pub certificate_mapping_incarnation: u64,
    pub certificate_mapping_generation: u64,
    pub certificate_instruction_generation: u64,
    pub certificate_authority_identity: u64,
    pub certificate_authority_generation: u64,
    pub certificate_run_generation: u64,
    pub certificate_view_index: u64,
    pub certificate_write_policy: u64,
    pub certificate_cache_identity: u64,
    pub certificate_token: u64,
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
    assert!(std::mem::offset_of!(Aarch64Cpu, active_authority) == 1872);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_valid) == 1880);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_view_count) == 1888);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_views) == 1896);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_mapping_incarnation) == 1992);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_authority) == 2000);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_trip) == 2008);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_decrement) == 2016);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_instruction_count) == 2024);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_iterations) == 2032);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_budget_iterations) == 2040);
    assert!(std::mem::offset_of!(Aarch64Cpu, loop_executable) == 2048);
    assert!(std::mem::offset_of!(Aarch64Cpu, active_view_incarnation) == 2056);
    assert!(std::mem::offset_of!(Aarch64Cpu, active_view_authority) == 2064);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_fast) == 2072);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_full) == 2080);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_guard_fallback) == 2088);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_reserved) == 2096);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_overflow) == 2104);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_committed) == 2112);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_dirty_merged) == 2120);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_view_publication) == 2128);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_write_policy) == 2192);
    assert!(std::mem::offset_of!(Aarch64Cpu, memory_write_index) == 2200);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_guest_first) == 2208);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_guest_last) == 2216);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_host_first) == 2224);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_data_permissions) == 2232);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_mapped_executable) == 2240);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_mapping_incarnation) == 2248);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_mapping_generation) == 2256);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_instruction_generation) == 2264);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_authority_identity) == 2272);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_authority_generation) == 2280);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_run_generation) == 2288);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_view_index) == 2296);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_write_policy) == 2304);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_cache_identity) == 2312);
    assert!(std::mem::offset_of!(Aarch64Cpu, certificate_token) == 2320);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_ibtc_authenticated_entries) == 2328);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_ibtc_shared_hits) == 2336);
    assert!(std::mem::offset_of!(Aarch64Cpu, diagnostic_ibtc_auth_rejections) == 2344);
    assert!(std::mem::offset_of!(Aarch64Cpu, code_arena_lower) == 2352);
    assert!(std::mem::offset_of!(Aarch64Cpu, code_arena_upper) == 2360);
    assert!(std::mem::offset_of!(Aarch64Cpu, entry_certificate_identity) == 2368);
    assert!(std::mem::offset_of!(Aarch64Cpu, fault_completed) == 2376);
    assert!(std::mem::offset_of!(Aarch64Cpu, ibtc_base) == 2384);
    assert!(std::mem::offset_of!(Aarch64Cpu, execution_identity) == 2392);
    assert!(std::mem::offset_of!(Aarch64Cpu, read_valid_count) == 2400);
    assert!(std::mem::offset_of!(Aarch64Cpu, reserve_filter) == 2408);
    assert!(std::mem::size_of::<Aarch64Cpu>() == 2472);

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
    assert!(std::mem::offset_of!(X86_64Cpu, vector_upper) == 1568);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_guest_first) == 1824);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_guest_last) == 1832);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_host_first) == 1840);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_data_permissions) == 1848);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_mapped_executable) == 1856);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_mapping_incarnation) == 1864);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_mapping_generation) == 1872);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_instruction_generation) == 1880);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_authority_identity) == 1888);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_authority_generation) == 1896);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_run_generation) == 1904);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_view_index) == 1912);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_write_policy) == 1920);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_cache_identity) == 1928);
    assert!(std::mem::offset_of!(X86_64Cpu, certificate_token) == 1936);
    assert!(std::mem::size_of::<X86_64Cpu>() == 1944);

    assert!(std::mem::align_of::<X86_64Cpu>() == 8);
};
