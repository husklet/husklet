use super::Error;

pub(super) fn archive_context(
    dockerfile: &[u8],
    files: &[(&str, &[u8])],
) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut archive = tar::Builder::new(&mut bytes);
    append(&mut archive, "Dockerfile", dockerfile)?;
    for (path, contents) in files {
        append(&mut archive, path, contents)?;
    }
    archive.finish()?;
    drop(archive);
    Ok(bytes)
}

pub(super) fn append(
    archive: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes)
}
