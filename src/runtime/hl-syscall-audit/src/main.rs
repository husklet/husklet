#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    arm: Option<u16>,
    x86: Option<u16>,
    name: String,
    category: &'static str,
    arm_category: &'static str,
    x86_category: &'static str,
}

#[derive(Default)]
struct Routes {
    arm: BTreeSet<(u16, String)>,
    x86: BTreeSet<(u16, String)>,
}

struct Audit {
    root: PathBuf,
}

fn main() {
    let audit = Audit::discover(PathBuf::from("."));
    let entries = audit.inventory().unwrap_or_else(|error| panic!("{error}"));
    let manifest = audit.render_manifest(&entries);
    let report = audit.render_report(&entries);
    let manifest_path = audit.root.join("syscall-inventory.tsv");
    let report_path = audit.root.join("SYSCALL_PARITY.md");
    if std::env::args().any(|argument| argument == "--check") {
        audit.check(&manifest_path, &manifest);
        audit.check(&report_path, &report);
    } else {
        fs::create_dir_all(manifest_path.parent().expect("manifest parent")).expect("create manifest directory");
        fs::write(manifest_path, manifest).expect("write manifest");
        fs::write(report_path, report).expect("write report");
    }
}

impl Audit {
    fn discover(mut root: PathBuf) -> Self {
        root = root.canonicalize().expect("canonical working directory");
        while !root.join("src/runtime/hl-linux").is_dir() {
            assert!(root.pop(), "workspace root not found");
        }
        Self { root }
    }

    fn check(&self, path: &Path, expected: &str) {
        let actual = fs::read_to_string(path).unwrap_or_default();
        assert_eq!(actual, expected, "{} is stale; run hl-syscall-audit", path.display());
    }

    fn inventory(&self) -> Result<Vec<Entry>, String> {
        let c = fs::read_to_string(self.root.join("../engine/src/linux_abi/number.c"))
            .map_err(|error| format!("read C number oracle: {error}"))?;
        let rust = fs::read_to_string(self.root.join("src/runtime/hl-linux/src/syscall/table.rs"))
            .map_err(|error| format!("read Rust router table: {error}"))?;
        let app = fs::read_to_string(self.root.join("src/app/hl-engine/src/ffi/linux/execution/ports.rs"))
            .map_err(|error| format!("read production inventory: {error}"))?;
        let router = self.rust_routes(&rust);
        let supported = self.supported_names(&app);
        let mut entries = self.c_entries(&c)?;
        for entry in &mut entries {
            entry.arm_category = Self::isa_status(entry.arm, &entry.name, &router.arm, &supported);
            entry.x86_category = Self::isa_status(entry.x86, &entry.name, &router.x86, &supported);
            entry.category = Self::aggregate(entry.arm_category, entry.x86_category);
        }
        let existing = entries
            .iter()
            .filter_map(|entry| entry.x86.map(|number| (number, entry.name.clone())))
            .collect::<BTreeSet<_>>();
        for (number, name) in &router.x86 {
            if existing.contains(&(*number, name.clone())) {
                continue;
            }
            let x86_category = Self::isa_status(Some(*number), name, &router.x86, &supported);
            entries.push(Entry {
                arm: None,
                x86: Some(*number),
                name: name.clone(),
                category: Self::aggregate("not-applicable", x86_category),
                arm_category: "not-applicable",
                x86_category,
            });
        }
        entries.sort_by_key(|entry| (entry.arm.unwrap_or(u16::MAX), entry.x86, entry.name.clone()));
        Ok(entries)
    }

    fn rust_routes(&self, source: &str) -> Routes {
        let mut routes = Routes::default();
        for line in source.lines().filter_map(Self::canonical_route) {
            routes.arm.insert((line.0, line.2.clone()));
            routes.x86.insert((line.1, line.2));
        }
        let mut legacy = None;
        for line in source.lines() {
            let line = line.trim();
            if line.contains("LegacyDefinition { raw_number:") {
                legacy = Self::legacy_number(line);
                continue;
            }
            let Some(number) = legacy else { continue };
            let Some(name) = Self::legacy_name(line) else { continue };
            routes.x86.insert((number, name));
            legacy = None;
        }
        routes
    }

    fn legacy_number(line: &str) -> Option<u16> {
        let raw = line.split("LegacyDefinition { raw_number:").nth(1)?;
        raw.split(',').next()?.trim().parse().ok()
    }

    fn legacy_name(line: &str) -> Option<String> {
        let raw = line.split("name:").nth(1)?.split(',').next()?;
        Some(raw.trim().trim_matches('"').to_owned())
    }

    fn canonical_route(line: &str) -> Option<(u16, u16, String)> {
        let line = line.trim();
        if !line.starts_with('(') {
            return None;
        }
        let fields = line
            .trim_matches(|value| value == '(' || value == ')' || value == ',')
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>();
        if fields.len() < 3 {
            return None;
        }
        Some((
            fields[0].parse().ok()?,
            fields[1].parse().ok()?,
            fields[2].trim_matches('"').to_owned(),
        ))
    }

    fn isa_status(
        number: Option<u16>,
        name: &str,
        routes: &BTreeSet<(u16, String)>,
        supported: &BTreeSet<&str>,
    ) -> &'static str {
        let Some(number) = number else { return "not-applicable" };
        if !routes.contains(&(number, name.to_owned())) {
            return "missing";
        }
        if supported.contains(name) {
            "supported"
        } else {
            "router-domain-only"
        }
    }

    fn aggregate(arm: &str, x86: &str) -> &'static str {
        let applicable = [arm, x86]
            .into_iter()
            .filter(|status| *status != "not-applicable")
            .collect::<Vec<_>>();
        if applicable.iter().any(|status| *status == "missing") {
            "missing"
        } else if applicable.iter().all(|status| *status == "supported") {
            "supported"
        } else {
            "router-domain-only"
        }
    }

    fn supported_names<'a>(&self, source: &'a str) -> BTreeSet<&'a str> {
        let block = source
            .split("pub(super) const SUPPORTED")
            .nth(1)
            .and_then(|value| value.split("];").next())
            .unwrap_or("");
        block
            .lines()
            .filter_map(|line| {
                let start = line.find('"')? + 1;
                let end = line[start..].find('"')? + start;
                Some(&line[start..end])
            })
            .collect()
    }

    fn c_entries(&self, source: &str) -> Result<Vec<Entry>, String> {
        let mut output = Vec::new();
        let mut pending = None;
        for line in source.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("case ") {
                let encoded = rest.split(':').next().unwrap_or("");
                pending = encoded.parse::<u16>().ok();
            }
            let Some(x86) = pending else { continue };
            let Some(returned) = trimmed.split("return ").nth(1) else {
                continue;
            };
            let Some(encoded) = returned.split(';').next() else {
                continue;
            };
            let Ok(arm) = encoded.trim().parse() else { continue };
            let Some(comment) = trimmed.split("//").nth(1) else {
                continue;
            };
            let Some(name) = comment.split_whitespace().next() else {
                continue;
            };
            output.push(Entry {
                arm: Some(arm),
                x86: Some(x86),
                name: name.trim().to_owned(),
                category: "missing",
                arm_category: "missing",
                x86_category: "missing",
            });
            pending = None;
        }
        if output.is_empty() {
            return Err("C number oracle produced no entries".into());
        }
        output.sort_by_key(|entry| (entry.arm, entry.x86));
        Ok(output)
    }

    fn render_manifest(&self, entries: &[Entry]) -> String {
        let mut output = "arm64\tx86_64\tname\tstatus\tarm64_status\tx86_64_status\n".to_owned();
        for entry in entries {
            let arm = entry.arm.map_or_else(|| "-".to_owned(), |value| value.to_string());
            let x86 = entry.x86.map_or_else(|| "-".to_owned(), |value| value.to_string());
            output.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                arm, x86, entry.name, entry.category, entry.arm_category, entry.x86_category
            ));
        }
        output
    }

    fn render_report(&self, entries: &[Entry]) -> String {
        let count = |category| entries.iter().filter(|entry| entry.category == category).count();
        let names = |category| {
            entries
                .iter()
                .filter(|entry| entry.category == category)
                .map(|entry| format!("`{}`", entry.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "# Production syscall parity\n\nGenerated by `cargo run -p hl-syscall-audit`. Do not edit manually.\n\nThis compares the retained C x86-64-to-AArch64 number oracle with the typed Rust router table and the syscalls explicitly wired by production `GuestExecutor`. Typed domain implementation is not counted as production support. `syscall-inventory.tsv` retains independent ARM64 and x86-64 status so an ISA-only legacy syscall cannot promote a different canonical operation.\n\n| Status | Count | Meaning |\n|---|---:|---|\n| supported | {} | Wired on every applicable guest ISA |\n| router-domain-only | {} | Typed on an applicable ISA but not fully production-advertised |\n| missing | {} | Missing or number/name-divergent on at least one applicable ISA |\n\n## Supported\n\n{}\n\n## Missing from the typed router\n\n{}\n",
            count("supported"),
            count("router-domain-only"),
            count("missing"),
            names("supported"),
            names("missing")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_parsers_classify() {
        let audit = Audit::discover(PathBuf::from("."));
        let entries = audit.inventory().unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "read" && entry.category == "supported")
        );
        assert!(entries.iter().any(|entry| entry.category == "router-domain-only"));
        assert!(entries.windows(2).all(|pair| {
            (pair[0].arm.unwrap_or(u16::MAX), pair[0].x86) <= (pair[1].arm.unwrap_or(u16::MAX), pair[1].x86)
        }));
        let wait = entries.iter().find(|entry| entry.name == "epoll_wait").unwrap();
        assert_eq!(wait.arm_category, "not-applicable");
        assert_eq!(wait.x86_category, "supported");
        let pwait = entries.iter().find(|entry| entry.name == "epoll_pwait").unwrap();
        assert_eq!(pwait.arm_category, "router-domain-only");
        assert_eq!(pwait.x86_category, "router-domain-only");
    }

    #[test]
    fn checked_outputs_current() {
        let audit = Audit::discover(PathBuf::from("."));
        let entries = audit.inventory().unwrap();
        assert_eq!(
            fs::read_to_string(audit.root.join("syscall-inventory.tsv")).unwrap(),
            audit.render_manifest(&entries),
        );
        assert_eq!(
            fs::read_to_string(audit.root.join("SYSCALL_PARITY.md")).unwrap(),
            audit.render_report(&entries),
        );
    }
}
