use std::collections::BTreeSet;

const ENTRY_LIMIT: usize = 4096;
const FIELD_LIMIT: usize = 4096;
const OPTION_LIMIT: usize = 64;

/// One validated Linux mount namespace record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountEntry {
    id: u32,
    parent: u32,
    major: u32,
    minor: u32,
    root: Vec<u8>,
    point: Vec<u8>,
    options: Vec<Vec<u8>>,
    optional: Vec<Vec<u8>>,
    filesystem: Vec<u8>,
    source: Vec<u8>,
    super_options: Vec<Vec<u8>>,
}

impl MountEntry {
    /// Creates one bounded record. Paths are escaped when rendered; option
    /// tokens must already represent single Linux mountinfo fields.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        id: u32,
        parent: u32,
        device: (u32, u32),
        root: Vec<u8>,
        point: Vec<u8>,
        options: Vec<Vec<u8>>,
        optional: Vec<Vec<u8>>,
        filesystem: Vec<u8>,
        source: Vec<u8>,
        super_options: Vec<Vec<u8>>,
    ) -> Option<Self> {
        if id == 0
            || !Self::path(&root)
            || !Self::path(&point)
            || !Self::field(&filesystem)
            || !Self::value(&source)
            || !Self::tokens(&options, true)
            || !Self::tokens(&optional, false)
            || !Self::tokens(&super_options, true)
        {
            return None;
        }
        Some(Self {
            id,
            parent,
            major: device.0,
            minor: device.1,
            root,
            point,
            options,
            optional,
            filesystem,
            source,
            super_options,
        })
    }

    fn path(bytes: &[u8]) -> bool {
        bytes.first() == Some(&b'/') && bytes.len() <= FIELD_LIMIT && !bytes.contains(&0)
    }

    fn field(bytes: &[u8]) -> bool {
        Self::value(bytes) && !bytes.iter().any(u8::is_ascii_whitespace)
    }

    fn value(bytes: &[u8]) -> bool {
        !bytes.is_empty() && bytes.len() <= FIELD_LIMIT && !bytes.contains(&0)
    }

    fn tokens(tokens: &[Vec<u8>], required: bool) -> bool {
        (!required || !tokens.is_empty())
            && tokens.len() <= OPTION_LIMIT
            && tokens
                .iter()
                .all(|token| Self::field(token) && token.as_slice() != b"-" && !token.contains(&b','))
    }

    fn render(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(self.id.to_string().as_bytes());
        output.push(b' ');
        output.extend_from_slice(self.parent.to_string().as_bytes());
        output.push(b' ');
        output.extend_from_slice(self.major.to_string().as_bytes());
        output.push(b':');
        output.extend_from_slice(self.minor.to_string().as_bytes());
        output.push(b' ');
        Self::escaped(output, &self.root);
        output.push(b' ');
        Self::escaped(output, &self.point);
        output.push(b' ');
        Self::joined(output, &self.options);
        for field in &self.optional {
            output.push(b' ');
            output.extend_from_slice(field);
        }
        output.extend_from_slice(b" - ");
        output.extend_from_slice(&self.filesystem);
        output.push(b' ');
        Self::escaped(output, &self.source);
        output.push(b' ');
        Self::joined(output, &self.super_options);
        output.push(b'\n');
    }

    fn render_mounts(&self, output: &mut Vec<u8>) {
        Self::escaped(output, &self.source);
        output.push(b' ');
        Self::escaped(output, &self.point);
        output.push(b' ');
        output.extend_from_slice(&self.filesystem);
        output.push(b' ');
        let mut options = self.options.clone();
        let has_access = options.iter().any(|option| matches!(option.as_slice(), b"ro" | b"rw"));
        for option in &self.super_options {
            if (has_access && matches!(option.as_slice(), b"ro" | b"rw")) || options.contains(option) {
                continue;
            }
            options.push(option.clone());
        }
        Self::joined(output, &options);
        output.extend_from_slice(b" 0 0\n");
    }

    fn render_stats(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(b"device ");
        Self::escaped(output, &self.source);
        output.extend_from_slice(b" mounted on ");
        Self::escaped(output, &self.point);
        output.extend_from_slice(b" with fstype ");
        output.extend_from_slice(&self.filesystem);
        output.push(b'\n');
    }

    fn joined(output: &mut Vec<u8>, tokens: &[Vec<u8>]) {
        for (index, token) in tokens.iter().enumerate() {
            if index != 0 {
                output.push(b',');
            }
            output.extend_from_slice(token);
        }
    }

    fn escaped(output: &mut Vec<u8>, bytes: &[u8]) {
        for byte in bytes {
            match byte {
                b' ' => output.extend_from_slice(b"\\040"),
                b'\t' => output.extend_from_slice(b"\\011"),
                b'\n' => output.extend_from_slice(b"\\012"),
                b'\\' => output.extend_from_slice(b"\\134"),
                byte => output.push(*byte),
            }
        }
    }
}

/// Coherent, ordered mount namespace snapshot rendered by procfs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountView {
    entries: Vec<MountEntry>,
}

impl MountView {
    /// Rejects oversized views and duplicate mount identities.
    #[must_use]
    pub fn new(entries: Vec<MountEntry>) -> Option<Self> {
        if entries.len() > ENTRY_LIMIT {
            return None;
        }
        let mut identities = BTreeSet::new();
        if entries.iter().any(|entry| !identities.insert(entry.id)) {
            return None;
        }
        Some(Self { entries })
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for entry in &self.entries {
            entry.render(&mut output);
        }
        output
    }

    /// Renders the same namespace snapshot in Linux's six-column fstab form.
    pub(super) fn mounts_bytes(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for entry in &self.entries {
            entry.render_mounts(&mut output);
        }
        output
    }

    pub(super) fn stats(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for entry in &self.entries {
            entry.render_stats(&mut output);
        }
        output
    }
}

#[cfg(test)]
mod test {
    use super::{MountEntry, MountView};

    fn entry(id: u32, point: &[u8]) -> MountEntry {
        MountEntry::new(
            id,
            0,
            (0, 1),
            b"/".to_vec(),
            point.to_vec(),
            vec![b"rw".to_vec()],
            Vec::new(),
            b"tmpfs".to_vec(),
            b"tmpfs".to_vec(),
            vec![b"rw".to_vec()],
        )
        .unwrap()
    }

    #[test]
    fn validates_identity() {
        assert!(MountView::new(vec![entry(1, b"/")]).is_some());
        assert!(MountView::new(vec![entry(1, b"/"), entry(1, b"/dev")]).is_none());
        assert!(
            MountEntry::new(
                0,
                0,
                (0, 1),
                b"/".to_vec(),
                b"/".to_vec(),
                vec![b"rw".to_vec()],
                Vec::new(),
                b"tmpfs".to_vec(),
                b"tmpfs".to_vec(),
                vec![b"rw".to_vec()],
            )
            .is_none()
        );
    }

    #[test]
    fn escapes_paths() {
        let view = MountView::new(vec![entry(1, b"/a b\\c")]).unwrap();
        assert_eq!(view.bytes(), b"1 0 0:1 / /a\\040b\\134c rw - tmpfs tmpfs rw\n");
        assert_eq!(view.mounts_bytes(), b"tmpfs /a\\040b\\134c tmpfs rw 0 0\n");
        assert_eq!(
            view.stats(),
            b"device tmpfs mounted on /a\\040b\\134c with fstype tmpfs\n"
        );
    }
}
