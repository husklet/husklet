use super::BuildError;
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

pub(super) struct Context<'a> {
    root: &'a Path,
}

impl<'a> Context<'a> {
    pub(super) fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &'a Path {
        self.root
    }

    pub(super) fn path(&self, value: &str) -> Result<PathBuf, BuildError> {
        let relative = Path::new(value.trim_start_matches('/'));
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(BuildError::Copy(value.into()));
        }
        let path = self.root.join(relative).canonicalize()?;
        if !path.starts_with(self.root.canonicalize()?) {
            return Err(BuildError::Copy(value.into()));
        }
        Ok(path)
    }

    pub(super) fn source(&self, value: &str) -> Result<PathBuf, BuildError> {
        let relative = Path::new(value.trim_start_matches('/'));
        if relative == Path::new(".") {
            return self.root.canonicalize().map_err(Into::into);
        }
        if relative.components().any(|part| {
            !matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        }) {
            return Err(BuildError::Copy(value.into()));
        }
        let path = self.root.join(relative).canonicalize()?;
        if !path.starts_with(self.root.canonicalize()?) {
            return Err(BuildError::Copy(value.into()));
        }
        Ok(path)
    }

    pub(super) fn read(&self, value: &str) -> Result<String, BuildError> {
        String::from_utf8(std::fs::read(self.path(value)?)?).map_err(|_| BuildError::Dockerfile)
    }

    pub(super) fn ignore(&self, dockerfile: &str) -> Result<(), BuildError> {
        let path = self.root.join(".dockerignore");
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        let rules = IgnoreRules::parse(&contents);
        let mut paths = self.descendants()?;
        paths.sort_by(|left, right| {
            right
                .components()
                .count()
                .cmp(&left.components().count())
                .then_with(|| left.cmp(right))
        });
        let evaluated = paths
            .into_iter()
            .map(|path| {
                let relative = path.strip_prefix(self.root).expect("descendant");
                let display = relative.to_string_lossy();
                let protected = display == dockerfile || display == ".dockerignore";
                let ignored = !protected && rules.ignored(&display);
                (path, ignored)
            })
            .collect::<Vec<_>>();
        for (path, ignored) in &evaluated {
            let keeps_child = path.is_dir()
                && evaluated
                    .iter()
                    .any(|(child, ignored)| !ignored && child != path && child.starts_with(path));
            if *ignored && !keeps_child && path.symlink_metadata().is_ok() {
                if path.is_dir() {
                    std::fs::remove_dir_all(path)?;
                } else {
                    std::fs::remove_file(path)?;
                }
            }
        }
        for protected in [self.root.join(dockerfile), path] {
            if protected.symlink_metadata().is_ok() {
                std::fs::remove_file(protected)?;
            }
        }
        Ok(())
    }

    pub(super) fn digest(&self) -> Result<[u8; 32], BuildError> {
        let mut paths = self.descendants()?;
        paths.sort();
        let mut digest = Sha256::new();
        for path in paths {
            let relative = path.strip_prefix(self.root).expect("context descendant");
            digest.update(relative.as_os_str().as_encoded_bytes());
            digest.update([0]);
            let metadata = std::fs::symlink_metadata(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
                digest.update(metadata.permissions().mode().to_le_bytes());
                digest.update(metadata.nlink().to_le_bytes());
            }
            if metadata.file_type().is_symlink() {
                digest.update(b"symlink");
                digest.update(std::fs::read_link(path)?.as_os_str().as_encoded_bytes());
            } else if metadata.is_dir() {
                digest.update(b"directory");
            } else {
                digest.update(b"file");
                digest.update(std::fs::read(path)?);
            }
            digest.update([0]);
        }
        Ok(digest.finalize().into())
    }

    fn descendants(&self) -> Result<Vec<PathBuf>, BuildError> {
        let mut paths = Vec::new();
        let mut directories = vec![self.root.to_owned()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    directories.push(path.clone());
                }
                paths.push(path);
            }
        }
        Ok(paths)
    }
}

struct IgnoreRules(Vec<IgnoreRule>);

struct IgnoreRule {
    include: bool,
    pattern: String,
}

impl IgnoreRules {
    fn parse(contents: &str) -> Self {
        Self(
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && *line != ".")
                .filter_map(|line| {
                    if line.starts_with('#') {
                        return None;
                    }
                    let literal = line
                        .strip_prefix('\\')
                        .filter(|pattern| pattern.starts_with('#') || pattern.starts_with('!'));
                    let (include, pattern) = literal.map_or_else(
                        || {
                            line.strip_prefix('!')
                                .map_or((false, line), |pattern| (true, pattern))
                        },
                        |literal| (false, literal),
                    );
                    let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
                    (!pattern.is_empty()).then(|| IgnoreRule {
                        include,
                        pattern: pattern.into(),
                    })
                })
                .collect(),
        )
    }

    pub(super) fn ignored(&self, path: &str) -> bool {
        self.0.iter().fold(false, |ignored, rule| {
            if Pattern::new(&rule.pattern).matches(path) {
                !rule.include
            } else {
                ignored
            }
        })
    }
}

pub(super) struct Pattern<'a> {
    value: &'a str,
}

impl<'a> Pattern<'a> {
    pub(super) fn new(value: &'a str) -> Self {
        Self { value }
    }

    pub(super) fn matches(&self, value: &str) -> bool {
        if self.value.is_empty() {
            return false;
        }
        let path = value
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if !self.value.contains('/') {
            return path.iter().any(|part| self.matches_segment(part));
        }
        let pattern = self
            .value
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        (1..=path.len()).any(|length| {
            let mut memo = vec![None; (length + 1) * (pattern.len() + 1)];
            Self::matches_path(&path[..length], &pattern, 0, 0, &mut memo)
        })
    }

    fn matches_path(
        path: &[&str],
        pattern: &[&str],
        path_index: usize,
        pattern_index: usize,
        memo: &mut [Option<bool>],
    ) -> bool {
        let width = pattern.len() + 1;
        let slot = path_index * width + pattern_index;
        if let Some(result) = memo[slot] {
            return result;
        }
        let result = match pattern.get(pattern_index) {
            None => path_index == path.len(),
            Some(&"**") => {
                Self::matches_path(path, pattern, path_index, pattern_index + 1, memo)
                    || (path_index < path.len()
                        && Self::matches_path(path, pattern, path_index + 1, pattern_index, memo))
            }
            Some(segment) => {
                path_index < path.len()
                    && Self::matches_segment_with(path[path_index].as_bytes(), segment.as_bytes())
                    && Self::matches_path(path, pattern, path_index + 1, pattern_index + 1, memo)
            }
        };
        memo[slot] = Some(result);
        result
    }

    fn matches_segment(&self, value: &str) -> bool {
        Self::matches_segment_with(value.as_bytes(), self.value.as_bytes())
    }

    fn matches_segment_with(value: &[u8], pattern: &[u8]) -> bool {
        let mut memo = vec![None; (value.len() + 1) * (pattern.len() + 1)];
        Self::matches_segment_at(value, pattern, 0, 0, &mut memo)
    }

    fn matches_segment_at(
        value: &[u8],
        pattern: &[u8],
        value_index: usize,
        pattern_index: usize,
        memo: &mut [Option<bool>],
    ) -> bool {
        let width = pattern.len() + 1;
        let slot = value_index * width + pattern_index;
        if let Some(result) = memo[slot] {
            return result;
        }
        let result = match pattern.get(pattern_index) {
            None => value_index == value.len(),
            Some(b'*') => {
                Self::matches_segment_at(value, pattern, value_index, pattern_index + 1, memo)
                    || (value_index < value.len()
                        && Self::matches_segment_at(
                            value,
                            pattern,
                            value_index + 1,
                            pattern_index,
                            memo,
                        ))
            }
            Some(b'?') => {
                value_index < value.len()
                    && Self::matches_segment_at(
                        value,
                        pattern,
                        value_index + 1,
                        pattern_index + 1,
                        memo,
                    )
            }
            Some(b'\\') => {
                value_index < value.len()
                    && pattern.get(pattern_index + 1) == value.get(value_index)
                    && Self::matches_segment_at(
                        value,
                        pattern,
                        value_index + 1,
                        pattern_index + 2,
                        memo,
                    )
            }
            Some(b'[') => pattern[pattern_index + 1..]
                .iter()
                .position(|byte| *byte == b']')
                .is_some_and(|end| {
                    value_index < value.len()
                        && Self::matches_class(
                            value[value_index],
                            &pattern[pattern_index + 1..pattern_index + 1 + end],
                        )
                        && Self::matches_segment_at(
                            value,
                            pattern,
                            value_index + 1,
                            pattern_index + end + 2,
                            memo,
                        )
                }),
            Some(byte) => {
                value.get(value_index) == Some(byte)
                    && Self::matches_segment_at(
                        value,
                        pattern,
                        value_index + 1,
                        pattern_index + 1,
                        memo,
                    )
            }
        };
        memo[slot] = Some(result);
        result
    }

    fn matches_class(value: u8, class: &[u8]) -> bool {
        let (negated, class) = class
            .first()
            .filter(|byte| matches!(byte, b'!' | b'^'))
            .map_or((false, class), |_| (true, &class[1..]));
        let mut matched = false;
        let mut index = 0;
        while index < class.len() {
            if index + 2 < class.len() && class[index + 1] == b'-' {
                matched |= (class[index]..=class[index + 2]).contains(&value);
                index += 3;
            } else {
                matched |= class[index] == value;
                index += 1;
            }
        }
        matched != negated
    }
}
