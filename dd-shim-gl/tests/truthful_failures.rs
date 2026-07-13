//! Phase-0 truthful-failure tests for the generated long-tail entry points.
//!
//! These prove the exit-gate behavior for GL: no unsupported entry point silently reports success.
//!   (a) a representative `stub` raises the API-correct GL/EGL error (not success) and initializes its
//!       outputs; a `partial` no-op initializes outputs but raises no error.
//!   (b) `DD_SHIM_STRICT=1` aborts the process on the FIRST stub call.
//!   (c) the advertised `glGetString(GL_VERSION)` matches the inventory's coherent (ES 2.0) profile.

use dd_shim_gl::glconst::*;

// The C-ABI entry points live at crate root (generated) / in the `gles`/`egl` modules (hand-written).
use dd_shim_gl::gles::{glGetError, glGetString};

/// (a) Truthful failures: stub raises the right error + zeroes outputs; partial zeroes outputs, no error.
///
/// All error-flag assertions live in this single test so the process-global GL error flag is never
/// raced by a parallel test.
#[test]
fn stub_raises_error_and_initializes_outputs() {
    // Drain any pre-existing error.
    let _ = glGetError();

    // --- partial getter: zeroes its output, raises NO error (spec-default degraded query) ---
    let mut params: i32 = 0x5555_5555u32 as i32;
    // glGetActiveUniformBlockiv(program, uniformBlockIndex, pname, params*)
    dd_shim_gl::glGetActiveUniformBlockiv(1, 0, 0, &mut params as *mut i32);
    assert_eq!(params, 0, "partial getter must initialize its output to 0");
    assert_eq!(glGetError(), GL_NO_ERROR, "a partial no-op must not raise a GL error");

    // --- stub with an out array: raises GL_INVALID_OPERATION and zeroes every handle ---
    let mut ids: [u32; 2] = [0xAAAA_AAAA, 0xBBBB_BBBB];
    dd_shim_gl::glGenProgramPipelines(2, ids.as_mut_ptr());
    assert_eq!(ids, [0, 0], "stub must initialize its out handles to 0 (no garbage)");
    assert_eq!(
        glGetError(),
        GL_INVALID_OPERATION,
        "an unsupported GL stub must raise GL_INVALID_OPERATION, not succeed silently"
    );
    // Error flag is read-and-cleared.
    assert_eq!(glGetError(), GL_NO_ERROR);

    // --- stub with a pointer return: returns null AND raises the error ---
    let sync = dd_shim_gl::glFenceSync(0x9117 /* GL_SYNC_GPU_COMMANDS_COMPLETE */, 0);
    assert!(sync.is_null(), "an unsupported sync create must return null, not a fake handle");
    assert_eq!(glGetError(), GL_INVALID_OPERATION);
    let _ = glGetError();

    // --- "not found" query: returns the correct sentinel (not a false slot-0 hit), no error ---
    let cname = b"Blk\0";
    let idx = dd_shim_gl::glGetUniformBlockIndex(1, cname.as_ptr() as *const core::ffi::c_char);
    assert_eq!(idx, GL_INVALID_INDEX, "unknown uniform block must return GL_INVALID_INDEX, not 0");
    assert_eq!(glGetError(), GL_NO_ERROR);

    // --- EGL stub: returns EGL_NO_* handle and raises an EGL error ---
    let img = dd_shim_gl::eglCreateImage(
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        0,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(img.is_null(), "an unsupported EGL create must return null, not a fake handle");
    assert_eq!(
        dd_shim_gl::egl::eglGetError(),
        EGL_BAD_ACCESS,
        "an unsupported EGL stub must raise EGL_BAD_ACCESS"
    );
}

/// (c) Advertisement matches the inventory's coherent profile: ES 2.0, NOT ES 3.x (whose symbols exist
/// only as truthful-failure stubs).
#[test]
fn advertised_gl_version_matches_inventory_profile() {
    let ptr = glGetString(0x1F02); // GL_VERSION
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const core::ffi::c_char) };
    let v = s.to_str().unwrap();
    assert_eq!(v, dd_shim_gl::ADVERTISED_GL_VERSION_STR);
    assert_eq!(v, "OpenGL ES 2.0 dd-shim", "must advertise the coherent ES 2.0 profile, not ES 3.x");
    assert_eq!(dd_shim_gl::ADVERTISED_GL_MAJOR, 2);
    assert_eq!(dd_shim_gl::ADVERTISED_GL_MINOR, 0);

    // The extension string advertises only host-backed extensions, and its count is consistent.
    let ext_ptr = glGetString(0x1F03); // GL_EXTENSIONS
    let ext = unsafe { std::ffi::CStr::from_ptr(ext_ptr as *const core::ffi::c_char) }.to_str().unwrap();
    let tokens: Vec<&str> = ext.split_whitespace().collect();
    assert_eq!(tokens.len(), dd_shim_gl::ADVERTISED_GL_EXTENSION_COUNT);

    // The indexed enumeration (glGetStringi) must return EXACTLY the same tokens, in order — the two
    // extension queries can never disagree (both come from the one inventory list).
    for (i, tok) in tokens.iter().enumerate() {
        let p = dd_shim_gl::gles::glGetStringi(0x1F03, i as u32);
        let s = unsafe { std::ffi::CStr::from_ptr(p as *const core::ffi::c_char) }.to_str().unwrap();
        assert_eq!(&s, tok, "glGetStringi({i}) must match token {i} of GL_EXTENSIONS");
    }
    // Out-of-range index returns the empty string, never garbage.
    let oob = dd_shim_gl::gles::glGetStringi(0x1F03, tokens.len() as u32);
    let oob = unsafe { std::ffi::CStr::from_ptr(oob as *const core::ffi::c_char) }.to_str().unwrap();
    assert_eq!(oob, "");
}

// ---- (b) DD_SHIM_STRICT aborts on the first stub call ------------------------------------------
//
// Driven by re-executing this test binary: the parent spawns the `strict_child_aborts_on_stub` test
// with DD_SHIM_STRICT=1 and a marker env var; the child calls a stub, which aborts (SIGABRT). The
// parent asserts the child did NOT exit successfully.

const CHILD_MARKER: &str = "DD_SHIM_GL_STRICT_CHILD";

#[test]
fn strict_child_aborts_on_stub() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        // Normal test run: this is the no-op guard. The real behavior is exercised by the parent below.
        return;
    }
    // In the child (DD_SHIM_STRICT=1): the first stub call must abort before we reach the line after it.
    dd_shim_gl::glDispatchCompute(1, 1, 1);
    // If strict mode did NOT abort, exit 0 so the parent's `!success()` assertion fails loudly.
    std::process::exit(0);
}

#[test]
fn dd_shim_strict_aborts_on_first_stub_call() {
    let exe = std::env::current_exe().expect("current_exe");
    let status = std::process::Command::new(exe)
        .args(["--exact", "strict_child_aborts_on_stub", "--nocapture"])
        .env("DD_SHIM_STRICT", "1")
        .env(CHILD_MARKER, "1")
        .status()
        .expect("spawn child");
    assert!(
        !status.success(),
        "DD_SHIM_STRICT must abort the process on the first stub call (child exited successfully): {status:?}"
    );
    // On unix, an abort() is delivered as SIGABRT (signal 6), never a normal exit code.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.code(), None, "expected termination by signal (SIGABRT), got exit code");
        assert_eq!(status.signal(), Some(6), "expected SIGABRT (6)");
    }
}
