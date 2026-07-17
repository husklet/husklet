use std::process::Command;

use super::*;

pub(super) fn eval(
    chk: &Check,
    stdout: &str,
    code: i32,
    guest: &str,
    args: &[String],
    e: Engine,
) -> Result<(), String> {
    match chk {
        Check::Exit(want) => (code == *want)
            .then_some(())
            .ok_or_else(|| format!("exit {code} != {want}")),
        Check::Out(want) => (stdout == *want)
            .then_some(())
            .ok_or_else(|| format!("stdout {:?} != {:?}", stdout, want)),
        Check::OutHas(sub) => stdout
            .contains(sub)
            .then_some(())
            .ok_or_else(|| format!("stdout {:?} lacks {:?}", stdout, sub)),
        Check::Oracle => {
            // native ground truth: aarch64 runs directly; x86_64 runs under qemu-user.
            let o = match e {
                Engine::LinuxX86_64 => Command::new("timeout")
                    .arg("25")
                    .arg("qemu-x86_64")
                    .arg(guest)
                    .args(args)
                    .output(),
                _ => Command::new("timeout")
                    .arg("25")
                    .arg(guest)
                    .args(args)
                    .output(),
            }
            .map_err(|err| format!("oracle spawn: {err}"))?;
            let (eo, ec) = (strip_noise(&o.stdout), o.status.code().unwrap_or(-1));
            if eo != stdout || ec != code {
                Err(format!(
                    "oracle mismatch (jit {code}/{stdout:?} vs native {ec}/{eo:?})"
                ))
            } else {
                Ok(())
            }
        }
    }
}
