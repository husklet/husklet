use super::{Work, WorkKey, definition::input::ManifestPath};
use crate::suite::Error;
use std::path::{Component, Path, PathBuf};

pub(super) async fn calculate(work: &[Work]) -> Result<String, Error> {
    let inputs = work
        .iter()
        .map(|item| {
            let case = &item.app.cases[item.case_index];
            FingerprintInput {
                key: item.key.clone(),
                directory: item.app.directory.clone(),
                source: case.source.clone(),
                build_inputs: case.inputs.clone(),
                golden: case.golden.clone(),
                metadata: format!(
                    "{}\0{}\0{:?}\0{:?}\0{:?}\0{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
                    item.app.image,
                    item.app.compiler_name(item.target),
                    item.app.execution,
                    case.arguments,
                    case.environment,
                    case.timeout,
                    case.exit,
                    case.flags,
                    case.destination,
                    case.compat,
                    case.soak
                ),
            }
        })
        .collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || fingerprint_files(&inputs))
        .await?
        .map_err(Into::into)
}

struct FingerprintInput {
    key: WorkKey,
    directory: PathBuf,
    source: ManifestPath,
    build_inputs: Vec<ManifestPath>,
    golden: PathBuf,
    metadata: String,
}

fn fingerprint_files(inputs: &[FingerprintInput]) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    digest.update(b"husklet-runtime-fingerprint-v2");
    for input in inputs {
        hash_field(&mut digest, b"case")?;
        hash_field(&mut digest, input.key.id.as_bytes())?;
        hash_field(&mut digest, input.key.target.name().as_bytes())?;
        hash_field(&mut digest, input.metadata.as_bytes())?;
        hash_file(
            &mut digest,
            b"source",
            input.source.name(),
            &input.directory.join(input.source.native()),
        )?;
        hash_build_inputs(&mut digest, &input.directory, &input.build_inputs)?;
        let golden = input
            .golden
            .strip_prefix(&input.directory)
            .map_err(|_| format!("golden is outside runtime category: {}", input.golden.display()))?;
        let golden_name = portable_name(golden)?;
        hash_file(&mut digest, b"golden", &golden_name, &input.golden)?;
    }
    Ok(digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn hash_build_inputs(digest: &mut impl sha2::Digest, directory: &Path, inputs: &[ManifestPath]) -> Result<(), String> {
    let mut inputs = inputs.iter().collect::<Vec<_>>();
    inputs.sort();
    for input in inputs {
        hash_file(digest, b"input", input.name(), &directory.join(input.native()))?;
    }
    Ok(())
}

fn hash_file(digest: &mut impl sha2::Digest, role: &[u8], name: &str, path: &Path) -> Result<(), String> {
    hash_file_bytes(digest, role, name.as_bytes(), path)
}

fn hash_file_bytes(digest: &mut impl sha2::Digest, role: &[u8], name: &[u8], path: &Path) -> Result<(), String> {
    hash_field(digest, b"file")?;
    hash_field(digest, role)?;
    hash_field(digest, name)?;
    let bytes = std::fs::read(path).map_err(|error| format!("cannot fingerprint {}: {error}", path.display()))?;
    hash_field(digest, &bytes)
}

fn hash_field(digest: &mut impl sha2::Digest, bytes: &[u8]) -> Result<(), String> {
    let length = u64::try_from(bytes.len()).map_err(|_| "fingerprint field is too large".to_owned())?;
    digest.update(length.to_be_bytes());
    digest.update(bytes);
    Ok(())
}

fn portable_name(path: &Path) -> Result<String, String> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("fingerprint path is not UTF-8: {}", path.display())),
            _ => Err(format!("fingerprint path is not normalized: {}", path.display())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|segments| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{FingerprintInput, fingerprint_files};
    use crate::{
        runtime::{WorkKey, definition::input::ManifestPath},
        suite::Target,
    };
    use std::fs;

    fn fingerprint_input(directory: &std::path::Path, inputs: &[&str]) -> FingerprintInput {
        FingerprintInput {
            key: WorkKey {
                id: "runtime/fingerprint".to_owned(),
                target: Target::Arm64,
            },
            directory: directory.to_path_buf(),
            source: serde_yaml::from_str("source.c").unwrap(),
            build_inputs: inputs
                .iter()
                .map(|value| serde_yaml::from_str::<ManifestPath>(value).unwrap())
                .collect(),
            golden: directory.join("golden.out"),
            metadata: "metadata".to_owned(),
        }
    }

    #[test]
    fn fingerprint_frames_names_bytes_and_ignores_input_declaration_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("source.c"), "ab").unwrap();
        fs::write(directory.path().join("golden.out"), "c").unwrap();
        fs::write(directory.path().join("a.h"), "same").unwrap();
        fs::write(directory.path().join("b.h"), "same").unwrap();

        let ordered = fingerprint_files(&[fingerprint_input(directory.path(), &["a.h", "b.h"])]).unwrap();
        let reversed = fingerprint_files(&[fingerprint_input(directory.path(), &["b.h", "a.h"])]).unwrap();
        assert_eq!(ordered, reversed);

        let named_a = fingerprint_files(&[fingerprint_input(directory.path(), &["a.h"])]).unwrap();
        let named_b = fingerprint_files(&[fingerprint_input(directory.path(), &["b.h"])]).unwrap();
        assert_ne!(named_a, named_b);

        fs::write(directory.path().join("a.h"), "changed").unwrap();
        let changed = fingerprint_files(&[fingerprint_input(directory.path(), &["a.h"])]).unwrap();
        assert_ne!(named_a, changed);

        fs::write(directory.path().join("source.c"), "a").unwrap();
        fs::write(directory.path().join("golden.out"), "bc").unwrap();
        let repartitioned = fingerprint_files(&[fingerprint_input(directory.path(), &["a.h"])]).unwrap();
        assert_ne!(changed, repartitioned);
    }

    #[test]
    fn fingerprint_has_a_stable_framed_value() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("source.c"), [0, 1, 2, 3]).unwrap();
        fs::write(directory.path().join("golden.out"), b"golden\0bytes").unwrap();
        fs::write(directory.path().join("link.ld"), b"SECTIONS {}\n").unwrap();
        let value = fingerprint_files(&[fingerprint_input(directory.path(), &["link.ld"])]).unwrap();
        assert_eq!(
            value,
            "f87de28bf588ca618c962384c59dd8cd0092b9917d7c67ddbc8a138002f4f7ee"
        );
    }
}
