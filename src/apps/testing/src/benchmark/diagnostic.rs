use super::adapter;

pub(super) fn report_x86_diagnostics(repetition: usize, diagnostics: &adapter::X86Diagnostics) {
    eprint!(
        "diagnostic repeat={repetition} x86_public_exits={} x86_public_syscalls={} x86_public_epochs={} x86_syscall_vector_dirty={}",
        diagnostics.public_exits,
        diagnostics.public_syscalls,
        diagnostics.public_epochs,
        diagnostics.syscall_vector_dirty,
    );
    if let Some(share) = diagnostics.dirty_share_ppm() {
        eprint!(" x86_syscall_vector_dirty_ppm={share}");
    }
    eprintln!();
}
