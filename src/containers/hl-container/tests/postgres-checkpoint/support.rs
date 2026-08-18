use super::*;

pub(super) async fn read_until(
    session: &mut hl_container::Session,
    marker: &str,
    timeout: Duration,
) -> Result<String, Error> {
    bounded("persistent PostgreSQL client output", timeout, async {
        let mut output = Vec::new();
        while let Some(entry) = session.next().await? {
            if entry.stream == Stream::Stdout {
                output.extend(entry.bytes);
            }
            if String::from_utf8_lossy(&output).contains(marker) {
                return Ok::<String, Error>(String::from_utf8(output)?);
            }
        }
        Err(format!("persistent PostgreSQL client ended before {marker:?}").into())
    })
    .await
}

pub(super) async fn bounded<T, E>(
    label: &str,
    timeout: Duration,
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, Error>
where
    E: Into<Error>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| format!("{label} exceeded {timeout:?}"))?
        .map_err(Into::into)
}

pub(super) fn checkpoint_artifact_hash<'a>(
    state: &Path,
    namespaces: impl IntoIterator<Item = &'a String>,
) -> Result<String, Error> {
    fn visit(root: &Path, path: &Path, hash: &mut Sha256) -> Result<(usize, u64), Error> {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut files = 0;
        let mut bytes = 0;
        for entry in entries {
            let path = entry.path();
            hash.update(path.strip_prefix(root)?.as_os_str().as_encoded_bytes());
            if path.is_dir() {
                let nested = visit(root, &path, hash)?;
                files += nested.0;
                bytes += nested.1;
            } else if path.is_file() {
                files += 1;
                let mut file = std::fs::File::open(path)?;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let count = file.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes += count as u64;
                    hash.update(&buffer[..count]);
                }
            }
        }
        Ok((files, bytes))
    }
    let checkpoint_root = state.join("runtime/checkpoints");
    let mut artifacts = namespaces
        .into_iter()
        .map(|namespace| checkpoint_root.join(namespace))
        .collect::<Vec<_>>();
    artifacts.sort();
    let mut hash = Sha256::new();
    for artifact in artifacts {
        require(
            artifact.is_dir(),
            format!("checkpoint artifact is missing: {}", artifact.display()),
        )?;
        hash.update(artifact.strip_prefix(&checkpoint_root)?.as_os_str().as_encoded_bytes());
        let (files, bytes) = visit(&checkpoint_root, &artifact, &mut hash)?;
        require(
            files > 0 && bytes > 0,
            format!(
                "checkpoint artifact contains no nonempty regular files: {}",
                artifact.display()
            ),
        )?;
    }
    Ok(hex_digest(hash.finalize().as_slice()))
}

pub(super) fn file_hash(path: &Path) -> Result<String, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex_digest(hash.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

pub(super) fn bounded_text(bytes: &[u8]) -> String {
    const LIMIT: usize = 16 * 1024;
    if bytes.len() <= LIMIT {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let half = LIMIT / 2;
    let mut text = String::from_utf8_lossy(&bytes[..half]).into_owned();
    text.push_str(&format!("\n[{} bytes omitted]\n", bytes.len() - LIMIT));
    text.push_str(&String::from_utf8_lossy(&bytes[bytes.len() - half..]));
    text
}

pub(super) fn append_output(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    const LIMIT: usize = 1024 * 1024;
    require(
        output.len().saturating_add(bytes.len()) <= LIMIT,
        "diagnostic command output exceeded 1 MiB",
    )?;
    output.extend_from_slice(bytes);
    Ok(())
}

pub(super) fn verify_elf_machine(path: &Path, architecture: &str) -> Result<(), Error> {
    let mut header = [0_u8; 24];
    std::fs::File::open(path)?.read_exact(&mut header)?;
    require(&header[..4] == b"\x7fELF", format!("{} is not ELF", path.display()))?;
    require(header[4] == 2, format!("{} is not ELF64", path.display()))?;
    require(header[5] == 1, format!("{} is not little-endian ELF", path.display()))?;
    require(
        header[6] == 1,
        format!("{} has unsupported ELF identification version", path.display()),
    )?;
    require(
        u32::from_le_bytes([header[20], header[21], header[22], header[23]]) == 1,
        format!("{} has unsupported ELF header version", path.display()),
    )?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match architecture {
        "amd64" => 62,
        "arm64" => 183,
        value => return Err(format!("unsupported fixture architecture {value:?}").into()),
    };
    require(
        machine == expected,
        format!("{} has ELF machine {machine}, expected {expected}", path.display()),
    )
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
pub(super) fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub(super) fn guest() -> Result<Guest, Error> {
    match std::env::var("HL_SCENARIO_TARGET") {
        Ok(value) if value == "amd64" => Ok(Guest::X86_64),
        Ok(value) if value == "arm64" => Ok(Guest::Aarch64),
        Err(std::env::VarError::NotPresent) => Ok(Guest::Aarch64),
        Ok(value) => Err(format!("unsupported HL_SCENARIO_TARGET {value:?}").into()),
        Err(error) => Err(error.into()),
    }
}
pub(super) fn require(condition: bool, message: impl Into<String>) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into().into()) }
}
pub(super) fn finish(outcome: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (outcome, cleanup) {
        (Err(primary), Err(cleanup)) => Err(format!("{primary}; cleanup also failed: {cleanup}").into()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
