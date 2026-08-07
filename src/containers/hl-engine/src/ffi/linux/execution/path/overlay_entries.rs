use std::collections::BTreeSet;
use std::io;

use hl_runtime::GuestName;

const ENTRY_MAXIMUM: usize = 65_536;

/// One nofollow directory candidate from a single overlay layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Candidate {
    pub(super) name: GuestName,
    pub(super) kind: u8,
    pub(super) whiteout: bool,
}

/// One layer snapshot in upper-to-lower precedence order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct LayerDirectory {
    pub(super) entries: Vec<Candidate>,
    pub(super) opaque: bool,
}

/// A merged entry whose order is stable for one directory snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MergedEntry {
    pub(super) name: Vec<u8>,
    pub(super) kind: u8,
}

/// Merges layer and namespace candidates without exposing marker files.
pub(super) fn merge(layers: &[LayerDirectory], namespace: &[Candidate]) -> io::Result<Vec<MergedEntry>> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    let namespace_names = namespace
        .iter()
        .map(|candidate| candidate.name.as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    push_named(&mut seen, &mut output, b".", libc::DT_DIR)?;
    push_named(&mut seen, &mut output, b"..", libc::DT_DIR)?;
    for layer in layers {
        for candidate in layer
            .entries
            .iter()
            .filter(|candidate| !namespace_names.contains(candidate.name.as_bytes()))
        {
            merge_candidate(candidate, &mut seen, &mut output)?;
        }
        if layer.opaque {
            break;
        }
    }
    for candidate in namespace {
        merge_candidate(candidate, &mut seen, &mut output)?;
    }
    Ok(output)
}

fn merge_candidate(
    candidate: &Candidate,
    seen: &mut BTreeSet<Vec<u8>>,
    output: &mut Vec<MergedEntry>,
) -> io::Result<()> {
    let name = candidate.name.as_bytes();
    if !seen.insert(name.to_vec()) || candidate.whiteout {
        return Ok(());
    }
    push(output, name, candidate.kind)
}

fn push_named(seen: &mut BTreeSet<Vec<u8>>, output: &mut Vec<MergedEntry>, name: &[u8], kind: u8) -> io::Result<()> {
    seen.insert(name.to_vec());
    push(output, name, kind)
}

fn push(output: &mut Vec<MergedEntry>, name: &[u8], kind: u8) -> io::Result<()> {
    if output.len() == ENTRY_MAXIMUM {
        return Err(io::Error::from_raw_os_error(libc::EOVERFLOW));
    }
    output.push(MergedEntry {
        name: name.to_vec(),
        kind,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use hl_runtime::GuestName;

    use super::{Candidate, LayerDirectory, merge};

    fn entry(name: &[u8], kind: u8) -> Candidate {
        Candidate {
            name: GuestName::new(name).unwrap(),
            kind,
            whiteout: false,
        }
    }

    fn whiteout(name: &[u8]) -> Candidate {
        Candidate {
            name: GuestName::new(name).unwrap(),
            kind: libc::DT_UNKNOWN,
            whiteout: true,
        }
    }

    #[test]
    fn higher_entries_and_whiteouts_decide_lower_names() {
        let layers = [
            LayerDirectory {
                entries: vec![
                    entry(b"upper", libc::DT_REG),
                    whiteout(b"hidden"),
                    entry(b"same", libc::DT_LNK),
                ],
                opaque: false,
            },
            LayerDirectory {
                entries: vec![
                    entry(b"hidden", libc::DT_DIR),
                    entry(b"same", libc::DT_REG),
                    entry(b"lower", libc::DT_DIR),
                ],
                opaque: false,
            },
        ];

        let merged = merge(&layers, &[]).unwrap();
        let names: Vec<&[u8]> = merged.iter().map(|entry| entry.name.as_slice()).collect();
        assert_eq!(names, [b".".as_slice(), b"..", b"upper", b"same", b"lower"]);
        assert_eq!(merged[3].kind, libc::DT_LNK);
    }

    #[test]
    fn opaque_layer_stops_lower_scan_but_namespace_entries_remain_visible() {
        let layers = [
            LayerDirectory {
                entries: vec![entry(b"upper", libc::DT_REG)],
                opaque: true,
            },
            LayerDirectory {
                entries: vec![entry(b"lower", libc::DT_REG)],
                opaque: false,
            },
        ];
        let namespace = [entry(b"mount", libc::DT_DIR), entry(b"upper", libc::DT_DIR)];

        let merged = merge(&layers, &namespace).unwrap();
        let names: Vec<&[u8]> = merged.iter().map(|entry| entry.name.as_slice()).collect();
        assert_eq!(names, [b".".as_slice(), b"..", b"mount", b"upper"]);
        assert_eq!(merged[3].kind, libc::DT_DIR);
    }

    #[test]
    fn namespace_route_wins_over_upper_entry_and_whiteout() {
        let layers = [LayerDirectory {
            entries: vec![entry(b"mounted", libc::DT_REG), whiteout(b"projected")],
            opaque: false,
        }];
        let namespace = [entry(b"mounted", libc::DT_DIR), entry(b"projected", libc::DT_LNK)];
        let merged = merge(&layers, &namespace).unwrap();
        let values = merged
            .iter()
            .map(|entry| (entry.name.as_slice(), entry.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                (b".".as_slice(), libc::DT_DIR),
                (b"..".as_slice(), libc::DT_DIR),
                (b"mounted".as_slice(), libc::DT_DIR),
                (b"projected".as_slice(), libc::DT_LNK),
            ]
        );
    }
}
