//! Perf reporting for the dd-tests matrix — the oracle-vs-JIT slowdown table + summary + machine
//! readable dump. Fed one [`Row`] per (case × engine) that actually executed (skips carry no timing).
//!
//! The table is opt-in (`PERF=1 make test` / `make perf`); the correctness matrix above it is
//! unchanged. Columns are Group | Test | Arch | Oracle(ms) | JIT(ms) | Ratio× | Status, sorted by
//! Ratio× descending (slowest first). Cases without an Oracle check are "jit-only": timed, but with no
//! native baseline, so they have no ratio and sort after the rated rows.

use std::fmt::Write as _;
use std::path::Path;

/// One measured cell of the matrix.
pub struct Row {
    pub group: String,
    pub test: String,
    pub arch: String, // engine label, e.g. "linux/x86_64"
    pub oracle_ms: Option<u128>,
    pub jit_ms: u128,
    pub status: &'static str, // pass | fail | xfail | xpass
}

impl Row {
    /// jit_ms / oracle_ms — the slowdown vs native. `None` for jit-only (oracle-less) cases.
    pub fn ratio(&self) -> Option<f64> {
        match self.oracle_ms {
            Some(o) if o > 0 => Some(self.jit_ms as f64 / o as f64),
            // Oracle present but sub-millisecond: treat native as ~0.5ms so we still get a finite ratio.
            Some(_) => Some(self.jit_ms as f64 / 0.5),
            None => None,
        }
    }
}

/// Sort rows slowest-first: rated rows by ratio desc, then jit-only rows by jit_ms desc.
fn sorted(rows: &[Row]) -> Vec<&Row> {
    let mut v: Vec<&Row> = rows.iter().collect();
    v.sort_by(|a, b| match (a.ratio(), b.ratio()) {
        (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.jit_ms.cmp(&a.jit_ms),
    });
    v
}

fn fmt_ratio(r: Option<f64>) -> String {
    match r {
        Some(x) => format!("{x:.2}×"),
        None => "—".into(),
    }
}
fn fmt_oracle(o: Option<u128>) -> String {
    match o { Some(v) => v.to_string(), None => "—".into() }
}
fn status_color(s: &str) -> &'static str {
    match s { "pass" => "32", "fail" => "31", "xfail" => "33", "xpass" => "35", _ => "90" }
}

/// Render the full performance table (aligned, colored) to a String.
pub fn table(rows: &[Row]) -> String {
    let rows = sorted(rows);
    // Column widths (data-driven, with sane minimums / caps).
    let w_group = rows.iter().map(|r| r.group.len()).max().unwrap_or(5).clamp(5, 22);
    let w_test = rows.iter().map(|r| r.test.len()).max().unwrap_or(4).clamp(4, 24);
    let w_arch = rows.iter().map(|r| r.arch.len()).max().unwrap_or(4).clamp(4, 16);
    let (w_or, w_jit, w_ratio, w_st) = (10, 8, 8, 6);
    let mut s = String::new();
    let _ = writeln!(s, "\n\x1b[1mPERFORMANCE  (oracle vs JIT, slowest first)\x1b[0m");
    let _ = writeln!(s, "\x1b[1m{:<wg$}  {:<wt$}  {:<wa$}  {:>wo$}  {:>wj$}  {:>wr$}  {:<ws$}\x1b[0m",
        "Group", "Test", "Arch", "Oracle(ms)", "JIT(ms)", "Ratio×", "Status",
        wg = w_group, wt = w_test, wa = w_arch, wo = w_or, wj = w_jit, wr = w_ratio, ws = w_st);
    for r in &rows {
        let g: String = r.group.chars().take(w_group).collect();
        let t: String = r.test.chars().take(w_test).collect();
        let _ = writeln!(s, "{:<wg$}  {:<wt$}  {:<wa$}  {:>wo$}  {:>wj$}  {:>wr$}  \x1b[{sc}m{:<ws$}\x1b[0m",
            g, t, r.arch, fmt_oracle(r.oracle_ms), r.jit_ms, fmt_ratio(r.ratio()), r.status,
            wg = w_group, wt = w_test, wa = w_arch, wo = w_or, wj = w_jit, wr = w_ratio, ws = w_st,
            sc = status_color(r.status));
    }
    s
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}
fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() { return 0.0; }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((p * (v.len() as f64 - 1.0)).round() as usize).min(v.len() - 1);
    v[idx]
}
fn geomean(v: &[f64]) -> f64 {
    if v.is_empty() { return 0.0; }
    let s: f64 = v.iter().map(|x| x.max(1e-9).ln()).sum();
    (s / v.len() as f64).exp()
}

/// Render the SUMMARY block (counts, median/p90/geomean ratio, slowest 15).
pub fn summary(rows: &[Row]) -> String {
    let timed = rows.len();
    let mut ratios: Vec<f64> = rows.iter().filter_map(|r| r.ratio()).collect();
    let jit_only = rows.iter().filter(|r| r.oracle_ms.is_none()).count();
    let rated = ratios.len();
    let mut s = String::new();
    let _ = writeln!(s, "\n\x1b[1mSUMMARY\x1b[0m");
    let _ = writeln!(s, "  cases timed      : {timed}   (rated: {rated}, jit-only/oracle-less: {jit_only})");
    if rated > 0 {
        let mut r2 = ratios.clone();
        let med = median(&mut ratios);
        let p90 = percentile(&mut r2, 0.90);
        let gm = geomean(&r2);
        let _ = writeln!(s, "  ratio× (jit/native): median {med:.2}×   p90 {p90:.2}×   geomean {gm:.2}×");
    } else {
        let _ = writeln!(s, "  ratio× (jit/native): n/a (no oracle-checked cases in this run)");
    }
    // Slowest 15 (rated first, then jit-only by jit_ms).
    let top = sorted(rows);
    let _ = writeln!(s, "  slowest 15:");
    for r in top.iter().take(15) {
        let _ = writeln!(s, "    {:>8}  {:<20} {:<12} jit {:>6}ms  oracle {:>6}",
            fmt_ratio(r.ratio()), format!("{}/{}", r.group, r.test), r.arch, r.jit_ms, fmt_oracle(r.oracle_ms));
    }
    s
}

/// Escape a CSV field (RFC-4180-ish): quote if it contains comma/quote/newline.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

/// Write the machine-readable CSV (`perf.csv`) + JSON (`perf.json`) into `dir`. Returns their paths.
pub fn write_machine(dir: &Path, rows: &[Row]) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let rows = sorted(rows);
    // CSV
    let mut csv = String::from("group,test,arch,oracle_ms,jit_ms,ratio,status\n");
    for r in &rows {
        let ratio = r.ratio().map(|x| format!("{x:.4}")).unwrap_or_default();
        let _ = writeln!(csv, "{},{},{},{},{},{},{}",
            csv_field(&r.group), csv_field(&r.test), csv_field(&r.arch),
            r.oracle_ms.map(|v| v.to_string()).unwrap_or_default(), r.jit_ms, ratio, r.status);
    }
    let csv_path = dir.join("perf.csv");
    std::fs::write(&csv_path, csv)?;
    // JSON
    let mut json = String::from("[\n");
    for (i, r) in rows.iter().enumerate() {
        let ratio = r.ratio().map(|x| format!("{x:.4}")).unwrap_or_else(|| "null".into());
        let oracle = r.oracle_ms.map(|v| v.to_string()).unwrap_or_else(|| "null".into());
        let _ = write!(json,
            "  {{\"group\":\"{}\",\"test\":\"{}\",\"arch\":\"{}\",\"oracle_ms\":{},\"jit_ms\":{},\"ratio\":{},\"status\":\"{}\"}}{}\n",
            json_esc(&r.group), json_esc(&r.test), json_esc(&r.arch), oracle, r.jit_ms, ratio, r.status,
            if i + 1 < rows.len() { "," } else { "" });
    }
    json.push_str("]\n");
    let json_path = dir.join("perf.json");
    std::fs::write(&json_path, json)?;
    Ok((csv_path, json_path))
}

fn json_esc(s: &str) -> String { s.replace('\\', "\\\\").replace('"', "\\\"") }
