use std::collections::VecDeque;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_LIMIT: u64 = 10 * 1024 * 1024 * 1024;
const DEFAULT_MAX_ENTRIES: usize = 200_000;
const DEFAULT_MAX_DEPTH: usize = 4;

#[derive(Debug)]
struct Config {
    roots: Vec<PathBuf>,
    limit: u64,
    max_entries: usize,
    max_depth: usize,
    check: bool,
}

#[derive(Debug, Eq, PartialEq)]
struct Finding {
    path: PathBuf,
    bytes: u64,
    complete: bool,
}

fn main() {
    let config = match parse(env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("hl-storage-check: {message}");
            eprintln!(
                "usage: hl-storage-check [--check] [--limit-bytes N] [--max-entries N] [--max-depth N] [ROOT ...]"
            );
            std::process::exit(64);
        }
    };
    let (mut findings, exhausted) = inspect(&config);
    findings.sort_by(|left, right| right.bytes.cmp(&left.bytes).then_with(|| left.path.cmp(&right.path)));
    for finding in &findings {
        println!(
            "{}\t{}\t{}",
            finding.bytes,
            if finding.complete { "complete" } else { "bounded" },
            finding.path.display()
        );
    }
    if exhausted {
        eprintln!("hl-storage-check: traversal reached --max-entries; results are incomplete");
    }
    eprintln!(
        "hl-storage-check: {} oversized Cargo target tree(s); no files were changed",
        findings.len()
    );
    if config.check && (!findings.is_empty() || exhausted) {
        std::process::exit(2);
    }
}

fn parse(arguments: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        roots: Vec::new(),
        limit: DEFAULT_LIMIT,
        max_entries: DEFAULT_MAX_ENTRIES,
        max_depth: DEFAULT_MAX_DEPTH,
        check: false,
    };
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--check" => config.check = true,
            "--limit-bytes" => config.limit = value(&mut arguments, &argument)?,
            "--max-entries" => config.max_entries = value(&mut arguments, &argument)?,
            "--max-depth" => config.max_depth = value(&mut arguments, &argument)?,
            "--help" | "-h" => return Err("help requested".into()),
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            root => config.roots.push(PathBuf::from(root)),
        }
    }
    if config.roots.is_empty() {
        config
            .roots
            .push(env::current_dir().map_err(|error| error.to_string())?);
        config.roots.push(env::temp_dir());
    }
    if config.max_entries == 0 {
        return Err("--max-entries must be greater than zero".into());
    }
    Ok(config)
}

fn value<T: std::str::FromStr>(arguments: &mut impl Iterator<Item = String>, option: &str) -> Result<T, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} requires a value"))?
        .parse()
        .map_err(|_| format!("invalid value for {option}"))
}

fn inspect(config: &Config) -> (Vec<Finding>, bool) {
    let mut queue = config
        .roots
        .iter()
        .cloned()
        .map(|path| (path, 0))
        .collect::<VecDeque<_>>();
    let mut findings = Vec::new();
    let mut visited = 0;
    while let Some((directory, depth)) = queue.pop_front() {
        if visited >= config.max_entries {
            return (findings, true);
        }
        visited += 1;
        if directory.file_name().is_some_and(|name| name == "target") {
            let (bytes, complete, consumed) = directory_size(&directory, config.max_entries - visited);
            visited += consumed;
            if bytes >= config.limit {
                findings.push(Finding {
                    path: directory,
                    bytes,
                    complete,
                });
            }
            continue;
        }
        if depth >= config.max_depth {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&directory) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink()) {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }
    }
    (findings, false)
}

fn directory_size(root: &Path, budget: usize) -> (u64, bool, usize) {
    let mut queue = VecDeque::from([root.to_owned()]);
    let mut bytes = 0_u64;
    let mut consumed = 0;
    while let Some(directory) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(directory) else {
            return (bytes, false, consumed);
        };
        for entry in entries.flatten() {
            if consumed >= budget {
                return (bytes, false, consumed);
            }
            consumed += 1;
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                return (bytes, false, consumed);
            };
            if metadata.is_file() {
                bytes = bytes.saturating_add(allocated_bytes(&metadata));
            } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
                queue.push_back(entry.path());
            }
        }
    }
    (bytes, true, consumed)
}

#[cfg(unix)]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(test)]
mod tests {
    use super::{Config, inspect};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "hl-storage-check-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn reports_only_oversized_targets_without_changing_them() {
        let root = TestDirectory::new("report");
        let target = root.path().join("project/target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("artifact"), b"12345").unwrap();
        fs::create_dir_all(root.path().join("source")).unwrap();
        fs::write(root.path().join("source/large"), b"123456789").unwrap();
        let config = Config {
            roots: vec![root.path().to_owned()],
            limit: 5,
            max_entries: 100,
            max_depth: 4,
            check: false,
        };
        let (findings, exhausted) = inspect(&config);
        assert!(!exhausted);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, target);
        assert_eq!(fs::read(findings[0].path.join("artifact")).unwrap(), b"12345");
    }

    #[test]
    fn entry_budget_makes_incomplete_scan_explicit() {
        let root = TestDirectory::new("budget");
        fs::create_dir_all(root.path().join("a/b/c")).unwrap();
        let config = Config {
            roots: vec![root.path().to_owned()],
            limit: 0,
            max_entries: 1,
            max_depth: 4,
            check: false,
        };
        assert!(inspect(&config).1);
    }
}
