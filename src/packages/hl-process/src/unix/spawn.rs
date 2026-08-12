//! Child creation: standard and exact-environment spawn paths.

#![allow(unsafe_code)]

use super::{OwnedCommand, Process, SPAWN_LOCK};
use std::ffi::CString;
use std::fs::File;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::MutexGuard;

pub(super) struct SpawnResult {
    pub(super) process: Process,
    pub(super) stdout: File,
    pub(super) stderr: File,
}

pub(super) fn spawn(command: &OwnedCommand) -> std::io::Result<SpawnResult> {
    if command.environment().is_some() {
        spawn_exact(command)
    } else {
        spawn_standard(command)
    }
}

fn spawn_standard(command: &OwnedCommand) -> std::io::Result<SpawnResult> {
    let _spawn = spawn_lock()?;
    let mut command = command.standard();
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr was not piped"))?;
    Ok(SpawnResult {
        process: Process::Standard(child),
        stdout: OwnedFd::from(stdout).into(),
        stderr: OwnedFd::from(stderr).into(),
    })
}

fn spawn_exact(command: &OwnedCommand) -> std::io::Result<SpawnResult> {
    let _spawn = spawn_lock()?;
    let stdout = pipe()?;
    let stderr = pipe()?;
    let null = CString::new("/dev/null").expect("static path has no NUL");
    let program = resolve(command.program())?;
    let program = cstring(program.as_os_str().as_bytes(), "program")?;
    let mut arguments = Vec::with_capacity(command.arguments().count() + 1);
    arguments.push(program.clone());
    for argument in command.arguments() {
        arguments.push(cstring(argument.as_bytes(), "argument")?);
    }
    let mut argument_pointers = arguments
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();
    let environment = command
        .environment()
        .expect("exact spawn requires an exact environment")
        .iter()
        .map(|entry| cstring(entry.record(), "environment record"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut environment_pointers = environment
        .iter()
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(std::ptr::null_mut()))
        .collect::<Vec<_>>();

    let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
    // SAFETY: `actions` is aligned uninitialized storage exclusively owned by
    // this function. Successful initialization establishes the C object's
    // lifetime; no Rust references alias it and the call cannot unwind.
    check_spawn(unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) })?;
    // SAFETY: initialization above succeeded, so the uniquely owned C object is
    // valid until the matching destroy below. Moving the value does not expose
    // internal storage to Rust and no concurrent access exists.
    let mut actions = unsafe { actions.assume_init() };
    let configured = (|| {
        // SAFETY: `actions` is initialized and uniquely owned, and the NUL-terminated path outlives the call.
        check_spawn(unsafe {
            libc::posix_spawn_file_actions_addopen(
                &raw mut actions,
                libc::STDIN_FILENO,
                null.as_ptr(),
                libc::O_RDONLY,
                0,
            )
        })?;
        for (source, target) in [
            (stdout.1.as_raw_fd(), libc::STDOUT_FILENO),
            (stderr.1.as_raw_fd(), libc::STDERR_FILENO),
        ] {
            // SAFETY: `actions` is initialized and uniquely owned; source is a
            // live pipe descriptor and target is a standard descriptor. The C
            // object copies integers only, retains no Rust pointer, and cannot unwind.
            check_spawn(unsafe { libc::posix_spawn_file_actions_adddup2(&raw mut actions, source, target) })?;
        }
        for descriptor in [
            stdout.0.as_raw_fd(),
            stdout.1.as_raw_fd(),
            stderr.0.as_raw_fd(),
            stderr.1.as_raw_fd(),
        ] {
            // SAFETY: the initialized action list is uniquely owned and stores
            // only the descriptor value. Pipe owners remain live through spawn;
            // no concurrent mutation or unwind crosses this call.
            check_spawn(unsafe { libc::posix_spawn_file_actions_addclose(&raw mut actions, descriptor) })?;
        }
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = configured {
        // SAFETY: the initialized list is still uniquely owned and no spawn is
        // active. Destroy retains no pointer and cannot unwind.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut actions) };
        return Err(error);
    }

    let mut attributes = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
    // SAFETY: aligned storage is exclusively owned and becomes initialized only
    // on success. The call retains no Rust pointer and cannot unwind.
    let attribute_status = unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) };
    if attribute_status != 0 {
        // SAFETY: actions remains initialized and uniquely owned.
        unsafe { libc::posix_spawn_file_actions_destroy(&raw mut actions) };
        return Err(std::io::Error::from_raw_os_error(attribute_status));
    }
    // SAFETY: successful initialization established the C object's lifetime.
    let mut attributes = unsafe { attributes.assume_init() };
    let configured = (|| {
        // SAFETY: initialized attributes are uniquely owned; both setters copy
        // scalar values and retain no pointers. No concurrent access exists.
        check_spawn(unsafe {
            libc::posix_spawnattr_setflags(&raw mut attributes, libc::POSIX_SPAWN_SETPGROUP as i16)
        })?;
        // SAFETY: as above; group zero requests a new group led by the child.
        check_spawn(unsafe { libc::posix_spawnattr_setpgroup(&raw mut attributes, 0) })
    })();
    let mut pid = 0;
    let spawned = configured.and_then(|()| {
        // All C strings and pointer arrays are NUL-terminated and remain alive for the call.
        // Actions/attributes are initialized and unaliased; env order and duplicate pointers
        // are deliberately preserved.
        // SAFETY: the child receives independent kernel state and the FFI cannot unwind.
        check_spawn(unsafe {
            libc::posix_spawn(
                &raw mut pid,
                program.as_ptr(),
                &raw const actions,
                &raw const attributes,
                argument_pointers.as_mut_ptr(),
                environment_pointers.as_mut_ptr(),
            )
        })
    });
    // SAFETY: both initialized C objects are uniquely owned, no spawn call is
    // active, destruction retains no pointer, and neither call can unwind.
    unsafe {
        libc::posix_spawnattr_destroy(&raw mut attributes);
        libc::posix_spawn_file_actions_destroy(&raw mut actions);
    }
    spawned?;
    drop(stdout.1);
    drop(stderr.1);
    Ok(SpawnResult {
        process: Process::Exact(pid),
        stdout: stdout.0.into(),
        stderr: stderr.0.into(),
    })
}

fn spawn_lock() -> std::io::Result<MutexGuard<'static, ()>> {
    SPAWN_LOCK
        .lock()
        .map_err(|_| std::io::Error::other("host process spawn lock was poisoned"))
}

fn resolve(program: &std::ffi::OsStr) -> std::io::Result<PathBuf> {
    if program.as_bytes().contains(&b'/') {
        return Ok(PathBuf::from(program));
    }
    let path = std::env::var_os("PATH")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "host PATH is unset"))?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        let bytes = candidate.as_os_str().as_bytes();
        let Ok(candidate_c) = CString::new(bytes) else {
            continue;
        };
        // SAFETY: candidate_c is a live NUL-terminated path. access observes
        // filesystem metadata, retains no pointer or Rust alias, and cannot unwind.
        if unsafe { libc::access(candidate_c.as_ptr(), libc::X_OK) } == 0 {
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("host executable {} was not found in PATH", program.display()),
    ))
}

fn cstring(bytes: &[u8], kind: &str) -> std::io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("process {kind} contains NUL")))
}

fn check_spawn(status: i32) -> std::io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status))
    }
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    // SAFETY: descriptors is aligned writable storage for exactly two integers.
    // The kernel writes no aliases and retains no pointer; the call cannot unwind.
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    // SAFETY: as above. `SPAWN_LOCK` is held across creation, CLOEXEC mutation,
    // and spawn, preventing another hl-process launch from observing the gap.
    let result = unsafe { libc::pipe(descriptors.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    for descriptor in descriptors {
        // SAFETY: descriptor is valid and live; F_SETFD copies the integer flag,
        // retains no pointer, the package spawn lock excludes sibling launches,
        // and the call cannot unwind.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            // SAFETY: both raw descriptors are uniquely owned on this error path.
            unsafe {
                libc::close(descriptors[0]);
                libc::close(descriptors[1]);
            }
            return Err(std::io::Error::last_os_error());
        }
    }
    let read = match relocate(descriptors[0]) {
        Ok(read) => read,
        Err(error) => {
            // SAFETY: the second descriptor remains uniquely owned because the
            // first relocation failed before wrapping it.
            unsafe { libc::close(descriptors[1]) };
            return Err(error);
        }
    };
    let write = match relocate(descriptors[1]) {
        Ok(write) => write,
        Err(error) => {
            drop(read);
            return Err(error);
        }
    };
    Ok((read, write))
}

fn relocate(descriptor: i32) -> std::io::Result<OwnedFd> {
    let descriptor = if descriptor <= libc::STDERR_FILENO {
        // SAFETY: descriptor is a valid uniquely owned pipe end. F_DUPFD_CLOEXEC
        // creates a second independently owned descriptor above stderr, retains
        // no pointer, and cannot unwind.
        let relocated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, libc::STDERR_FILENO + 1) };
        if relocated < 0 {
            let error = std::io::Error::last_os_error();
            // SAFETY: relocation failed, so the original descriptor remains
            // uniquely owned and must be closed on this error path.
            unsafe { libc::close(descriptor) };
            return Err(error);
        }
        // SAFETY: the original descriptor is uniquely owned and replaced by the
        // relocated descriptor; no alias or concurrent package spawn exists.
        unsafe { libc::close(descriptor) };
        relocated
    } else {
        descriptor
    };
    // SAFETY: this function has unique ownership of the live descriptor and
    // transfers it exactly once into OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}
