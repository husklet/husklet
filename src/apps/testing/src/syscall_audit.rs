#![forbid(unsafe_code)]

use clap::Args;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry {
    arm: Option<u16>,
    x86: Option<u16>,
    name: String,
}

struct Audit {
    root: PathBuf,
}

/// Syscall numbers pinned from the production C engine; refresh explicitly with `--regenerate`.
const NUMBERS: &str = include_str!("../syscall-audit/syscall-numbers.tsv");

#[derive(Args)]
pub struct Options {
    #[arg(long, conflicts_with = "regenerate")]
    check: bool,
    #[arg(long, value_name = "NUMBER_C")]
    regenerate: Option<PathBuf>,
}

pub fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let audit = Audit::discover(PathBuf::from("."));
    if let Some(oracle) = options.regenerate.as_deref() {
        audit.regenerate(oracle)?;
        return Ok(());
    }
    let _ = options.check;
    audit.inventory().unwrap_or_else(|error| panic!("{error}"));
    Ok(())
}

impl Audit {
    fn discover(mut root: PathBuf) -> Self {
        root = root.canonicalize().expect("canonical working directory");
        while !root.join("src/runtime/hl-native").is_dir() {
            assert!(root.pop(), "workspace root not found");
        }
        Self { root }
    }

    fn inventory(&self) -> Result<Vec<Entry>, String> {
        let production = fs::read_to_string(
            self.root
                .join("src/runtime/hl-native/src/linux_abi/number.c"),
        )
        .map_err(|error| format!("read production C syscall map: {error}"))?;
        let entries = Self::c_entries(&production)?;
        let checked = Self::numbers(NUMBERS)?;
        if entries != checked {
            return Err("syscall-numbers.tsv differs from production C number.c; run testing syscall-audit --regenerate src/runtime/hl-native/src/linux_abi/number.c".into());
        }
        Ok(entries)
    }

    /// Rewrites the checked-in number table from an explicitly supplied C oracle.
    fn regenerate(&self, oracle: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let source = fs::read_to_string(oracle)?;
        let entries = Self::c_entries(&source).map_err(std::io::Error::other)?;
        let mut output = String::from("# arm64\tx86_64\tname\n");
        for entry in entries {
            let _ = writeln!(
                output,
                "{}\t{}\t{}",
                entry.arm.expect("oracle arm number"),
                entry.x86.expect("oracle x86 number"),
                entry.name
            );
        }
        fs::write(self.numbers_path(), output)?;
        Ok(())
    }

    fn numbers_path(&self) -> PathBuf {
        self.root.join("src/apps/testing/syscall-audit/syscall-numbers.tsv")
    }

    fn numbers(source: &str) -> Result<Vec<Entry>, String> {
        let mut output = Vec::new();
        for line in source.lines().filter(|line| !line.starts_with('#')) {
            let columns = line.split('\t').collect::<Vec<_>>();
            let [arm, x86, name] = columns.as_slice() else {
                continue;
            };
            output.push(Entry {
                arm: Some(
                    arm.parse()
                        .map_err(|_| format!("invalid arm number: {arm}"))?,
                ),
                x86: Some(
                    x86.parse()
                        .map_err(|_| format!("invalid x86 number: {x86}"))?,
                ),
                name: (*name).to_owned(),
            });
        }
        if output.is_empty() {
            return Err("checked-in number table is empty".into());
        }
        output.sort_by_key(|entry| (entry.arm, entry.x86));
        Ok(output)
    }

    fn c_entries(source: &str) -> Result<Vec<Entry>, String> {
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
            let Ok(arm) = encoded.trim().parse() else {
                continue;
            };
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
            });
            pending = None;
        }
        if output.is_empty() {
            return Err("C number oracle produced no entries".into());
        }
        output.sort_by_key(|entry| (entry.arm, entry.x86));
        Ok(output)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_map_matches_checked_inventory() {
        let audit = Audit::discover(PathBuf::from("."));
        let entries = audit.inventory().unwrap();
        assert!(entries.iter().any(|entry| entry.name == "read"));
        assert!(entries.windows(2).all(|pair| {
            (pair[0].arm.unwrap_or(u16::MAX), pair[0].x86)
                <= (pair[1].arm.unwrap_or(u16::MAX), pair[1].x86)
        }));
    }
}
