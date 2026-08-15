use super::Error;
use crate::{runtime::image::TestImage, suite::Target};
use hl_container::ExitStatus;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const RETAINED_UPPER_LIMIT: u64 = 256 * 1024 * 1024;

pub(super) struct FailureRetention {
    root: PathBuf,
    token: String,
    retained: AtomicBool,
}

#[derive(Serialize)]
struct RetainedFailure<'a> {
    version: u16,
    case: &'a str,
    target: &'a str,
    attempt: u16,
    status: Option<ExitStatus>,
    image: &'a str,
    artifact_sha256: String,
    upper_tar_sha256: String,
    upper_tar_bytes: u64,
    rootfs: &'a hl_images::rootfs::Reference,
    lower_path: &'a Path,
    upper_path: &'a Path,
}

impl FailureRetention {
    pub(super) fn new(root: PathBuf, token: String) -> Self {
        Self {
            root,
            token,
            retained: AtomicBool::new(false),
        }
    }

    pub(super) fn retain(
        &self,
        fixture: &TestImage,
        artifact: &Path,
        case: &str,
        target: Target,
        attempt: u16,
        status: Option<ExitStatus>,
    ) -> Result<PathBuf, Error> {
        let Some(lower) = fixture.lower() else {
            return Err("failed root is not an overlay".into());
        };
        if self.retained.swap(true, Ordering::AcqRel) {
            return Err("this worker already retained its first failed overlay".into());
        }
        fs::create_dir_all(&self.root)?;
        let temporary = tempfile::Builder::new().prefix("retaining-").tempdir_in(&self.root)?;
        let archive_path = temporary.path().join("upper.tar");
        let archive = fs::File::create(&archive_path)?;
        let mut bounded = HashedBoundedWriter::new(archive, RETAINED_UPPER_LIMIT);
        fixture.archive_upper(&mut bounded)?;
        bounded.flush()?;
        let (upper_tar_bytes, upper_tar_sha256) = bounded.finish();
        let manifest = RetainedFailure {
            version: 1,
            case,
            target: target.name(),
            attempt,
            status,
            image: fixture.identity(),
            artifact_sha256: sha256_file(artifact)?,
            upper_tar_sha256,
            upper_tar_bytes,
            rootfs: fixture.reference(),
            lower_path: lower,
            upper_path: fixture.path(),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        fs::write(temporary.path().join("manifest.json"), bytes)?;
        let destination = self.root.join(&self.token);
        let temporary = temporary.keep();
        fs::rename(temporary, &destination)?;
        Ok(destination)
    }
}

pub(super) struct HashedBoundedWriter<W> {
    inner: W,
    digest: Sha256,
    bytes: u64,
    limit: u64,
}

impl<W> HashedBoundedWriter<W> {
    pub(super) fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
            bytes: 0,
            limit,
        }
    }

    pub(super) fn finish(self) -> (u64, String) {
        (self.bytes, hex_digest(self.digest.finalize().as_slice()))
    }
}

impl<W: std::io::Write> std::io::Write for HashedBoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if self.bytes.saturating_add(length) > self.limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "retained overlay exceeded 256 MiB",
            ));
        }
        self.inner.write_all(bytes)?;
        self.digest.update(bytes);
        self.bytes += length;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
