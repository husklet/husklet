use std::path::{Path, PathBuf};

pub const SOURCE_PATHS: &[&str] = &[
    "src/request.rs",
    "src/port.rs",
    "src/manifest.rs",
    "src/subscription.rs",
    "src/capability.rs",
    "../hl-gui/src/identity.rs",
    "../hl-gui/src/node/patch.rs",
    "../hl-gui/src/node/prop.rs",
    "../hl-gui/src/data/mod.rs",
    "../hl-gui/src/style.rs",
];

pub fn fingerprint<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in (part.len() as u64).to_le_bytes().iter().chain(part) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

pub fn source_fingerprint(manifest: &Path) -> Result<u64, String> {
    let sources = SOURCE_PATHS
        .iter()
        .map(|relative| {
            let path = manifest.join(relative);
            std::fs::read(&path).map(|bytes| ((*relative).to_owned(), bytes))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot read protocol source: {error}"))?;
    Ok(fingerprint(
        sources
            .iter()
            .flat_map(|(path, bytes)| [path.as_bytes(), bytes.as_slice()]),
    ))
}

pub fn verify(manifest: &Path) -> Result<(), String> {
    let document = std::fs::read(manifest.join("protocol/v1.json"))
        .map_err(|error| format!("cannot read protocol/v1.json: {error}"))?;
    let recorded = std::fs::read_to_string(manifest.join("protocol/v1.fnv1a64"))
        .map_err(|error| format!("cannot read protocol/v1.fnv1a64: {error}"))?;
    verify_contents(source_fingerprint(manifest)?, &document, recorded.trim())
}

fn verify_contents(source: u64, document: &[u8], recorded: &str) -> Result<(), String> {
    let text = std::str::from_utf8(document).map_err(|_| "protocol/v1.json is not UTF-8".to_owned())?;
    let marker = format!("\"source_fingerprint\": \"fnv1a64:{source:016x}\"");
    if !text.contains(&marker) {
        return Err("protocol/v1.json is stale relative to authoritative Rust sources".to_owned());
    }
    let actual = format!("{:016x}", fingerprint([document]));
    if recorded != actual {
        return Err("protocol/v1.json differs from its generated artifact fingerprint".to_owned());
    }
    Ok(())
}

pub fn watched(manifest: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    SOURCE_PATHS
        .iter()
        .map(|path| manifest.join(path))
        .chain([manifest.join("protocol/v1.json"), manifest.join("protocol/v1.fnv1a64")])
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, verify_contents};

    fn generated(source: u64) -> (Vec<u8>, String) {
        let document = format!("{{\n  \"source_fingerprint\": \"fnv1a64:{source:016x}\"\n}}\n").into_bytes();
        let recorded = format!("{:016x}", fingerprint([document.as_slice()]));
        (document, recorded)
    }

    #[test]
    fn source_and_artifact_drift_fail_but_regeneration_recovers() {
        let (document, recorded) = generated(7);
        assert!(verify_contents(8, &document, &recorded).unwrap_err().contains("stale"));

        let mut edited = document.clone();
        edited.push(b' ');
        assert!(verify_contents(7, &edited, &recorded).unwrap_err().contains("differs"));

        let (regenerated, current) = generated(8);
        verify_contents(8, &regenerated, &current).expect("regenerated pair");
    }
}
