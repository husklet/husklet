use hl_gpu::ir::{decode_stream, encode_stream, Cmd, Enc};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_TARGET_TEXTURE: u32 = 514;
const DEFAULT_WIDTH: u32 = 512;
const DEFAULT_HEIGHT: u32 = 256;
const DEFAULT_RADIUS: usize = 16;
const DEFAULT_STEP: usize = 1;

#[derive(Debug)]
struct Args {
    input: PathBuf,
    out_dir: PathBuf,
    hl_display: PathBuf,
    target: u32,
    width: u32,
    height: u32,
    submit_index: usize,
    center_op: Option<usize>,
    radius: usize,
    step: usize,
    counts: Option<Vec<usize>>,
    balance_pass: bool,
    keep_ir: bool,
    no_replay: bool,
}

#[derive(Clone, Debug)]
struct TargetPass {
    begin: usize,
    end: Option<usize>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum OpenPass {
    Render,
    Compute,
}

fn usage(code: i32) -> ! {
    eprintln!(
        "\
Usage:
  cargo run -p hl-gpu --example replay_ir_sweep -- <input.ir> <out-dir> [options]

Truncate a captured hl-gpu IR stream at a sweep of Submit encoder op counts, then
replay each trimmed stream through:
  target-chrome-codex/release/hl-display selftest-shim-ir <trim.ir> <out.png> <w> <h> <target>

This is meant for Chrome offscreen texture reduction. By default it targets
texture 514 at 512x256, finds the render pass writing that texture, sweeps op
counts around the end of that pass, and emits PNGs plus manifest.jsonl.

Options:
  --hl-display <path>   hl-display binary to invoke
                        [default: target-chrome-codex/release/hl-display]
  --target <id>         texture id wired as selftest render target [default: 514]
  --width <px>          IOSurface/readback width [default: 512]
  --height <px>         IOSurface/readback height [default: 256]
  --submit-index <n>    which top-level Submit to trim [default: 0]
  --center-op <n>       center sweep around this kept-op count
  --radius <n>          include center +/- n when --counts is absent [default: 16]
  --step <n>            sweep stride when --counts is absent [default: 1]
  --counts <spec>       explicit kept-op counts; examples: 33,41 or 24:48 or 24:48:2
  --no-balance-pass     leave partial render/compute passes open after truncation
  --keep-ir             keep every generated trim IR beside its PNG
  --no-replay           write trim IRs but do not run hl-display
  -h, --help            show this help

Notes:
  * Count N means keep the first N original encoder ops from the selected Submit.
  * With pass balancing enabled, a synthetic EndRenderPass/EndComputePass may be
    appended after the N kept ops so Metal replay can read back a valid target.
  * If --center-op is omitted, the center is the first complete render pass that
    writes --target, using its end+1 kept-op count. For texture 514 in the known
    Chrome capture this lands on keep=41 and includes the interesting keep=33
    partial-pass cut with the default radius."
    );
    std::process::exit(code);
}

fn parse_num<T: std::str::FromStr>(flag: &str, value: Option<OsString>) -> T {
    let value = value.unwrap_or_else(|| {
        eprintln!("{flag} needs a value");
        usage(2);
    });
    let s = value.to_string_lossy();
    s.parse().unwrap_or_else(|_| {
        eprintln!("{flag} expects a numeric value, got {s:?}");
        usage(2);
    })
}

fn parse_counts_spec(spec: &str) -> Vec<usize> {
    let mut out = BTreeSet::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let fields: Vec<&str> = part.split(':').collect();
        match fields.as_slice() {
            [one] => {
                out.insert(one.parse().unwrap_or_else(|_| {
                    eprintln!("bad --counts item {part:?}");
                    usage(2);
                }));
            }
            [start, end] | [start, end, _] => {
                let start: usize = start.parse().unwrap_or_else(|_| {
                    eprintln!("bad --counts range start in {part:?}");
                    usage(2);
                });
                let end: usize = end.parse().unwrap_or_else(|_| {
                    eprintln!("bad --counts range end in {part:?}");
                    usage(2);
                });
                let step = if fields.len() == 3 {
                    fields[2].parse().unwrap_or_else(|_| {
                        eprintln!("bad --counts range step in {part:?}");
                        usage(2);
                    })
                } else {
                    1
                };
                if step == 0 {
                    eprintln!("--counts range step must be > 0");
                    usage(2);
                }
                if start <= end {
                    let mut n = start;
                    while n <= end {
                        out.insert(n);
                        match n.checked_add(step) {
                            Some(next) => n = next,
                            None => break,
                        }
                    }
                } else {
                    let mut n = start;
                    while n >= end {
                        out.insert(n);
                        match n.checked_sub(step) {
                            Some(next) => n = next,
                            None => break,
                        }
                    }
                }
            }
            _ => {
                eprintln!("bad --counts item {part:?}");
                usage(2);
            }
        }
    }
    out.into_iter().collect()
}

fn parse_args() -> Args {
    let mut raw = std::env::args_os().skip(1);
    let first = raw.next().unwrap_or_else(|| usage(2));
    if first == "-h" || first == "--help" {
        usage(0);
    }
    let second = raw.next().unwrap_or_else(|| usage(2));
    let mut args = Args {
        input: PathBuf::from(first),
        out_dir: PathBuf::from(second),
        hl_display: PathBuf::from("target-chrome-codex/release/hl-display"),
        target: DEFAULT_TARGET_TEXTURE,
        width: DEFAULT_WIDTH,
        height: DEFAULT_HEIGHT,
        submit_index: 0,
        center_op: None,
        radius: DEFAULT_RADIUS,
        step: DEFAULT_STEP,
        counts: None,
        balance_pass: true,
        keep_ir: false,
        no_replay: false,
    };

    while let Some(flag) = raw.next() {
        match flag.to_string_lossy().as_ref() {
            "--hl-display" => {
                args.hl_display = PathBuf::from(raw.next().unwrap_or_else(|| usage(2)))
            }
            "--target" => args.target = parse_num("--target", raw.next()),
            "--width" => args.width = parse_num("--width", raw.next()),
            "--height" => args.height = parse_num("--height", raw.next()),
            "--submit-index" => args.submit_index = parse_num("--submit-index", raw.next()),
            "--center-op" => args.center_op = Some(parse_num("--center-op", raw.next())),
            "--radius" => args.radius = parse_num("--radius", raw.next()),
            "--step" => args.step = parse_num("--step", raw.next()),
            "--counts" => {
                let spec = raw.next().unwrap_or_else(|| usage(2));
                args.counts = Some(parse_counts_spec(&spec.to_string_lossy()));
            }
            "--no-balance-pass" => args.balance_pass = false,
            "--keep-ir" => args.keep_ir = true,
            "--no-replay" => {
                args.no_replay = true;
                args.keep_ir = true;
            }
            "-h" | "--help" => usage(0),
            other => {
                eprintln!("unknown option {other:?}");
                usage(2);
            }
        }
    }

    if args.step == 0 {
        eprintln!("--step must be > 0");
        usage(2);
    }
    args
}

fn submit_encoder(cmds: &[Cmd], submit_index: usize) -> Option<&[Enc]> {
    let mut seen = 0usize;
    for cmd in cmds {
        if let Cmd::Submit(cb) = cmd {
            if seen == submit_index {
                return Some(&cb.encoder);
            }
            seen += 1;
        }
    }
    None
}

fn submit_encoder_mut(cmds: &mut [Cmd], submit_index: usize) -> Option<&mut Vec<Enc>> {
    let mut seen = 0usize;
    for cmd in cmds {
        if let Cmd::Submit(cb) = cmd {
            if seen == submit_index {
                return Some(&mut cb.encoder);
            }
            seen += 1;
        }
    }
    None
}

fn target_passes(encoder: &[Enc], target: u32) -> Vec<TargetPass> {
    let mut out = Vec::new();
    let mut active: Option<usize> = None;
    for (i, op) in encoder.iter().enumerate() {
        match op {
            Enc::BeginRenderPass { color, .. } => {
                let writes_target = color.iter().any(|c| c.texture == target);
                if writes_target {
                    active = Some(out.len());
                    out.push(TargetPass {
                        begin: i,
                        end: None,
                    });
                } else {
                    active = None;
                }
            }
            Enc::EndRenderPass => {
                if let Some(idx) = active.take() {
                    out[idx].end = Some(i);
                }
            }
            _ => {}
        }
    }
    out
}

fn count_set(args: &Args, op_count: usize, passes: &[TargetPass]) -> Vec<usize> {
    let mut set = BTreeSet::new();
    if let Some(counts) = &args.counts {
        for &n in counts {
            set.insert(n.min(op_count));
        }
        return set.into_iter().collect();
    }

    let center = args
        .center_op
        .or_else(|| passes.iter().find_map(|p| p.end.map(|end| end + 1)))
        .unwrap_or(op_count)
        .min(op_count);
    let start = center.saturating_sub(args.radius);
    let end = center.saturating_add(args.radius).min(op_count);
    let mut n = start;
    while n <= end {
        set.insert(n);
        match n.checked_add(args.step) {
            Some(next) => n = next,
            None => break,
        }
    }

    for pass in passes {
        set.insert(pass.begin.min(op_count));
        if let Some(end) = pass.end {
            set.insert((end + 1).min(op_count));
        }
    }

    set.into_iter().collect()
}

fn append_balancing_end(encoder: &mut Vec<Enc>) -> Option<&'static str> {
    let mut stack = Vec::new();
    for op in encoder.iter() {
        match op {
            Enc::BeginRenderPass { .. } => stack.push(OpenPass::Render),
            Enc::EndRenderPass => {
                if stack.last() == Some(&OpenPass::Render) {
                    stack.pop();
                }
            }
            Enc::BeginComputePass => stack.push(OpenPass::Compute),
            Enc::EndComputePass => {
                if stack.last() == Some(&OpenPass::Compute) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }
    match stack.last().copied() {
        Some(OpenPass::Render) => {
            encoder.push(Enc::EndRenderPass);
            Some("EndRenderPass")
        }
        Some(OpenPass::Compute) => {
            encoder.push(Enc::EndComputePass);
            Some("EndComputePass")
        }
        None => None,
    }
}

fn make_trimmed(
    cmds: &[Cmd],
    submit_index: usize,
    keep_ops: usize,
    balance_pass: bool,
) -> (Vec<Cmd>, usize, Option<&'static str>) {
    let mut trimmed = cmds.to_vec();
    let encoder =
        submit_encoder_mut(&mut trimmed, submit_index).expect("submit disappeared while trimming");
    let kept = keep_ops.min(encoder.len());
    encoder.truncate(kept);
    let appended = if balance_pass {
        append_balancing_end(encoder)
    } else {
        None
    };
    (trimmed, kept, appended)
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._+-=:".contains(c))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn main() {
    let args = parse_args();
    let bytes =
        fs::read(&args.input).unwrap_or_else(|e| panic!("read {}: {e}", args.input.display()));
    let cmds =
        decode_stream(&bytes).unwrap_or_else(|e| panic!("decode {}: {e}", args.input.display()));
    let encoder = submit_encoder(&cmds, args.submit_index)
        .unwrap_or_else(|| panic!("input has no Submit at index {}", args.submit_index));
    let passes = target_passes(encoder, args.target);
    let counts = count_set(&args, encoder.len(), &passes);

    fs::create_dir_all(&args.out_dir)
        .unwrap_or_else(|e| panic!("mkdir {}: {e}", args.out_dir.display()));
    let manifest_path = args.out_dir.join("manifest.jsonl");
    let mut manifest = String::new();

    println!(
        "decoded {} bytes, {} top-level cmds, submit[{}] ops={}",
        bytes.len(),
        cmds.len(),
        args.submit_index,
        encoder.len()
    );
    if passes.is_empty() {
        println!("target texture {}: no render pass found", args.target);
    } else {
        for pass in &passes {
            println!(
                "target texture {} pass begin={} end={:?} keep_after_end={:?}",
                args.target,
                pass.begin,
                pass.end,
                pass.end.map(|n| n + 1)
            );
        }
    }
    println!(
        "sweeping {} count(s), target={} size={}x{}, balance_pass={}, replay={}",
        counts.len(),
        args.target,
        args.width,
        args.height,
        args.balance_pass,
        !args.no_replay
    );

    let mut failures = 0usize;
    for keep in counts {
        let (trimmed, kept, appended) =
            make_trimmed(&cmds, args.submit_index, keep, args.balance_pass);
        let trim_bytes = encode_stream(&trimmed);
        let stem = format!("keep-{kept:04}");
        let ir_path = args.out_dir.join(format!("{stem}.ir"));
        let png_path = args.out_dir.join(format!("{stem}-tex{}.png", args.target));
        let log_path = args.out_dir.join(format!("{stem}.log"));
        fs::write(&ir_path, &trim_bytes)
            .unwrap_or_else(|e| panic!("write {}: {e}", ir_path.display()));

        let mut exit_code = 0i32;
        let mut png_bytes = 0u64;
        let mut ir_kept = args.keep_ir || args.no_replay;
        if !args.no_replay {
            let output = Command::new(&args.hl_display)
                .arg("selftest-shim-ir")
                .arg(&ir_path)
                .arg(&png_path)
                .arg(args.width.to_string())
                .arg(args.height.to_string())
                .arg(args.target.to_string())
                .output()
                .unwrap_or_else(|e| panic!("run {}: {e}", args.hl_display.display()));

            let mut log = Vec::new();
            log.extend_from_slice(&output.stdout);
            log.extend_from_slice(&output.stderr);
            fs::write(&log_path, log)
                .unwrap_or_else(|e| panic!("write {}: {e}", log_path.display()));
            exit_code = output.status.code().unwrap_or(-1);
            if !output.status.success() {
                failures += 1;
                ir_kept = true;
            }
            png_bytes = fs::metadata(&png_path).map(|m| m.len()).unwrap_or(0);

            if !args.keep_ir && output.status.success() {
                let _ = fs::remove_file(&ir_path);
            }
        }

        println!(
            "keep={kept:04} appended={} exit={} png={} bytes={}{}",
            appended.unwrap_or("-"),
            exit_code,
            shell_quote(&png_path),
            png_bytes,
            if args.keep_ir || args.no_replay {
                format!(" ir={}", shell_quote(&ir_path))
            } else {
                String::new()
            }
        );

        manifest.push_str(&format!(
            "{{\"keep_ops\":{},\"encoded_ops\":{},\"appended\":\"{}\",\"ir\":\"{}\",\"ir_kept\":{},\"png\":\"{}\",\"log\":\"{}\",\"exit_code\":{},\"png_bytes\":{},\"ir_bytes\":{}}}\n",
            kept,
            submit_encoder(&trimmed, args.submit_index).map(|e| e.len()).unwrap_or(0),
            appended.unwrap_or(""),
            json_escape(&ir_path.to_string_lossy()),
            ir_kept,
            json_escape(&png_path.to_string_lossy()),
            json_escape(&log_path.to_string_lossy()),
            exit_code,
            png_bytes,
            trim_bytes.len()
        ));
    }

    fs::write(&manifest_path, manifest)
        .unwrap_or_else(|e| panic!("write {}: {e}", manifest_path.display()));
    println!("manifest={}", shell_quote(&manifest_path));

    if failures > 0 {
        eprintln!(
            "{failures} replay(s) failed; see per-count logs in {}",
            args.out_dir.display()
        );
        std::process::exit(1);
    }
}
