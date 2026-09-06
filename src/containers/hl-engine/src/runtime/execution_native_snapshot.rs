//! Stopped-process architectural capture for the staged native x86 checkpoint reader.
//!
//! This module deliberately has no publication or admission call site yet. It establishes only the
//! ptrace lifetime and the fixed register codec needed by a later whole-process capture transaction.
#![allow(dead_code)] // Intentionally staged before the coordinator transaction is wired to it.

use std::io;
#[cfg(target_arch = "x86_64")]
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"HLNXREG\0";
const VERSION: u16 = 1;
const ELF_MACHINE_X86_64: u16 = 62;
pub(super) const RECORD_SIZE: usize = 256;
const REGISTER_COUNT: usize = 27;
const REGISTER_OFFSET: usize = 32;
#[cfg(target_arch = "x86_64")]
const STOP_DEADLINE: Duration = Duration::from_secs(2);

/// Canonical `native-x86-v1` architectural state. Register order is Linux x86-64
/// `user_regs_struct`: r15..gs, exactly as returned by `NT_PRSTATUS`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct X86RegisterRecord {
    pub(super) signal_mask: u64,
    pub(super) registers: [u64; REGISTER_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InvalidRecord {
    Size,
    Magic,
    Version,
    Architecture,
    DeclaredSize,
    Reserved,
}

impl X86RegisterRecord {
    pub(super) fn encode(&self) -> [u8; RECORD_SIZE] {
        let mut bytes = [0_u8; RECORD_SIZE];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
        bytes[12..16].copy_from_slice(&(RECORD_SIZE as u32).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.signal_mask.to_le_bytes());
        for (index, value) in self.registers.iter().enumerate() {
            let start = REGISTER_OFFSET + index * 8;
            bytes[start..start + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, InvalidRecord> {
        let bytes: &[u8; RECORD_SIZE] = bytes.try_into().map_err(|_| InvalidRecord::Size)?;
        if &bytes[..8] != MAGIC {
            return Err(InvalidRecord::Magic);
        }
        if u16::from_le_bytes(bytes[8..10].try_into().expect("fixed field")) != VERSION {
            return Err(InvalidRecord::Version);
        }
        if u16::from_le_bytes(bytes[10..12].try_into().expect("fixed field")) != ELF_MACHINE_X86_64 {
            return Err(InvalidRecord::Architecture);
        }
        if u32::from_le_bytes(bytes[12..16].try_into().expect("fixed field")) as usize != RECORD_SIZE {
            return Err(InvalidRecord::DeclaredSize);
        }
        if bytes[24..32].iter().chain(&bytes[248..]).any(|byte| *byte != 0) {
            return Err(InvalidRecord::Reserved);
        }
        let mut registers = [0; REGISTER_COUNT];
        for (index, value) in registers.iter_mut().enumerate() {
            let start = REGISTER_OFFSET + index * 8;
            *value = u64::from_le_bytes(bytes[start..start + 8].try_into().expect("fixed field"));
        }
        Ok(Self {
            signal_mask: u64::from_le_bytes(bytes[16..24].try_into().expect("fixed field")),
            registers,
        })
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) fn capture(pid: libc::pid_t) -> io::Result<X86RegisterRecord> {
    capture_with(pid, || Ok(()))
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn capture(_pid: libc::pid_t) -> io::Result<X86RegisterRecord> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native-x86-v1 register capture requires a Linux x86-64 host",
    ))
}

#[cfg(target_arch = "x86_64")]
fn capture_with(pid: libc::pid_t, after_stop: impl FnOnce() -> io::Result<()>) -> io::Result<X86RegisterRecord> {
    if pid <= 1 || pid == unsafe { libc::getpid() } {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capture target must be another live process",
        ));
    }
    ptrace(libc::PTRACE_SEIZE, pid, 0, 0)?;
    let mut guard = TraceGuard {
        pid,
        was_group_stopped: false,
        ptrace_stopped: false,
    };
    ptrace(libc::PTRACE_INTERRUPT, pid, 0, 0)?;
    guard.was_group_stopped = wait_for_ptrace_stop(pid, STOP_DEADLINE)?;
    guard.ptrace_stopped = true;
    after_stop()?;

    let mut raw: libc::user_regs_struct = unsafe { std::mem::zeroed() };
    let mut iov = libc::iovec {
        iov_base: (&raw mut raw).cast(),
        iov_len: std::mem::size_of_val(&raw),
    };
    ptrace(
        libc::PTRACE_GETREGSET,
        pid,
        libc::NT_PRSTATUS as usize,
        (&raw mut iov) as usize,
    )?;
    if iov.iov_len != std::mem::size_of_val(&raw) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short x86-64 NT_PRSTATUS register set",
        ));
    }
    let mut signal_mask = 0_u64;
    ptrace(
        libc::PTRACE_GETSIGMASK,
        pid,
        std::mem::size_of_val(&signal_mask),
        (&raw mut signal_mask) as usize,
    )?;
    let registers = unsafe { std::ptr::read_unaligned((&raw const raw).cast::<[u64; REGISTER_COUNT]>()) };
    drop(guard);
    Ok(X86RegisterRecord { signal_mask, registers })
}

#[cfg(target_arch = "x86_64")]
fn process_is_stopped(pid: libc::pid_t) -> io::Result<bool> {
    let status = std::fs::read(format!("/proc/{pid}/status"))?;
    let state = status
        .split(|byte| *byte == b'\n')
        .find(|line| line.starts_with(b"State:"))
        .and_then(|line| line.get(7).copied())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process state"))?;
    Ok(matches!(state, b'T' | b't'))
}

#[cfg(target_arch = "x86_64")]
fn wait_for_ptrace_stop(pid: libc::pid_t, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        // Observe death without collecting it. The process owner must retain the exit status.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let observed = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WEXITED | libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
            )
        };
        if observed < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if unsafe { info.si_pid() } == pid
            && matches!(info.si_code, libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED)
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tracee exited before register capture",
            ));
        }

        let mut status = 0;
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::__WALL | libc::WNOHANG) };
        if waited < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if waited == pid && libc::WIFSTOPPED(status) {
            let event = status >> 16;
            if event != libc::PTRACE_EVENT_STOP {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected ptrace stop"));
            }
            // A seized tracee reports PTRACE_EVENT_STOP/SIGTRAP for PTRACE_INTERRUPT.
            // A pre-existing group-stop reports the actual stopping signal instead.
            return Ok(libc::WSTOPSIG(status) != libc::SIGTRAP);
        }
        if waited == pid {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected tracee event"));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for ptrace stop",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(target_arch = "x86_64")]
fn ptrace(request: libc::c_uint, pid: libc::pid_t, address: usize, data: usize) -> io::Result<()> {
    let result = unsafe { libc::ptrace(request, pid, address as *mut libc::c_void, data as *mut libc::c_void) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
struct TraceGuard {
    pid: libc::pid_t,
    was_group_stopped: bool,
    ptrace_stopped: bool,
}

#[cfg(target_arch = "x86_64")]
impl Drop for TraceGuard {
    fn drop(&mut self) {
        // Cleanup must not wait: a destructor cannot safely depend on an untrusted tracee making
        // another transition. A stopped tracee can be detached; otherwise this best-effort detach
        // either succeeds immediately or the kernel releases the relationship when the task exits.
        let signal = if self.was_group_stopped {
            libc::SIGSTOP as usize
        } else {
            0
        };
        if self.ptrace_stopped {
            let _ = ptrace(libc::PTRACE_DETACH, self.pid, 0, signal);
        } else {
            let _ = ptrace(libc::PTRACE_DETACH, self.pid, 0, 0);
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::os::fd::RawFd;
    use std::time::{Duration, Instant};

    #[test]
    fn canonical_codec_is_exact_and_rejects_every_structural_mutation() {
        let record = X86RegisterRecord {
            signal_mask: 0x1122_3344_5566_7788,
            registers: std::array::from_fn(|index| 0xfeed_0000_0000_0000 | index as u64),
        };
        let encoded = record.encode();
        assert_eq!(&encoded[..8], b"HLNXREG\0");
        assert_eq!(&encoded[8..10], &1_u16.to_le_bytes());
        assert_eq!(&encoded[10..12], &62_u16.to_le_bytes());
        assert_eq!(&encoded[12..16], &256_u32.to_le_bytes());
        assert_eq!(X86RegisterRecord::decode(&encoded), Ok(record));
        for (at, expected) in [
            (0, InvalidRecord::Magic),
            (8, InvalidRecord::Version),
            (10, InvalidRecord::Architecture),
            (12, InvalidRecord::DeclaredSize),
            (24, InvalidRecord::Reserved),
            (255, InvalidRecord::Reserved),
        ] {
            let mut changed = encoded;
            changed[at] ^= 1;
            assert_eq!(X86RegisterRecord::decode(&changed), Err(expected), "offset {at}");
        }
        assert_eq!(X86RegisterRecord::decode(&encoded[..255]), Err(InvalidRecord::Size));
    }

    #[test]
    fn live_child_register_sentinel_and_signal_mask_are_captured_without_leaving_it_stopped() {
        let (pid, ready) = sentinel_child(false);
        wait_byte(ready);
        let record = capture(pid).unwrap();
        assert_eq!(record.registers[0], 0x1515_1515_1515_1515, "r15 sentinel");
        assert_ne!(record.signal_mask & (1 << (libc::SIGUSR1 - 1)), 0);
        wait_until_running(pid);
        kill_and_reap(pid);
    }

    #[test]
    fn failed_capture_thaws_running_child_and_preserves_an_existing_group_stop() {
        let (running, ready) = sentinel_child(false);
        wait_byte(ready);
        let error = capture_with(running, || Err(io::Error::other("injected after stop"))).unwrap_err();
        assert_eq!(error.to_string(), "injected after stop");
        wait_until_running(running);
        kill_and_reap(running);

        let (stopped, ready) = sentinel_child(true);
        wait_byte(ready);
        wait_until_stopped(stopped);
        capture(stopped).unwrap();
        wait_until_stopped(stopped);
        unsafe { libc::kill(stopped, libc::SIGCONT) };
        kill_and_reap(stopped);
    }

    #[test]
    fn exit_observation_does_not_reap_the_process_owners_child() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe { libc::_exit(23) }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match wait_for_ptrace_stop(pid, Duration::from_millis(10)) {
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) && Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                other => panic!("unexpected exit observation: {other:?}"),
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 23);
    }

    #[test]
    fn seized_child_exit_is_observed_without_consuming_its_status() {
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            loop {
                unsafe { libc::pause() };
            }
        }
        ptrace(libc::PTRACE_SEIZE, pid, 0, 0).unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let error = wait_for_ptrace_stop(pid, Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
    }

    fn sentinel_child(stop: bool) -> (libc::pid_t, RawFd) {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                libc::close(pipe[0]);
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut mask);
                libc::sigaddset(&raw mut mask, libc::SIGUSR1);
                libc::sigprocmask(libc::SIG_BLOCK, &raw const mask, std::ptr::null_mut());
                std::arch::asm!("mov r15, {sentinel}", sentinel = in(reg) 0x1515_1515_1515_1515_u64);
                libc::write(pipe[1], b"R".as_ptr().cast(), 1);
                if stop {
                    libc::raise(libc::SIGSTOP);
                }
                loop {
                    libc::pause();
                }
            }
        }
        unsafe {
            libc::close(pipe[1]);
        }
        (pid, pipe[0])
    }

    fn wait_byte(fd: RawFd) {
        let mut byte = 0;
        assert_eq!(unsafe { libc::read(fd, (&raw mut byte as *mut u8).cast(), 1) }, 1);
        unsafe {
            libc::close(fd);
        }
    }

    fn wait_until_stopped(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if process_is_stopped(pid).unwrap_or(false) {
                return;
            }
            std::thread::yield_now();
        }
        panic!("child {pid} did not stop");
    }

    fn wait_until_running(pid: libc::pid_t) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !process_is_stopped(pid).unwrap_or(true) {
                let stable_until = Instant::now() + Duration::from_millis(50);
                while Instant::now() < stable_until {
                    assert!(!process_is_stopped(pid).unwrap(), "child {pid} stopped after detach");
                    std::thread::yield_now();
                }
                return;
            }
            std::thread::yield_now();
        }
        panic!("child {pid} did not resume");
    }

    fn kill_and_reap(pid: libc::pid_t) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
}
