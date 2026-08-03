//! Bounded, byte-exact GPU protocol captures for deterministic host replay.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_gpu::Cmd;

const MAGIC: &[u8; 8] = b"HLGPUCAP";
const COMPLETE: &[u8; 8] = b"COMPLETE";
const INCOMPLETE: &[u8; 8] = b"INCOMPLT";
const VERSION: u32 = 1;
const HEADER_BYTES: u64 = 12;
const FOOTER_BYTES: u64 = 8;

#[derive(Clone)]
pub(super) struct Config {
    directory: PathBuf,
    max_batches: u64,
    max_bytes: u64,
    presentations: u64,
    reserved: Arc<AtomicU64>,
}

impl Config {
    pub(super) fn configured() -> io::Result<Option<Self>> {
        let Some(options) = super::CaptureOptions::configured()? else {
            return Ok(None);
        };
        let directory = options.directory().to_owned();
        fs::create_dir_all(&directory)?;
        Ok(Some(Self {
            directory,
            max_batches: options.batches(),
            max_bytes: options.bytes(),
            presentations: options.presentations(),
            reserved: Arc::new(AtomicU64::new(0)),
        }))
    }

    pub(super) fn open(&self, connection: u64) -> io::Result<Capture> {
        Capture::open(self.clone(), connection)
    }

    #[cfg(test)]
    fn testing(directory: PathBuf, max_batches: u64, max_bytes: u64) -> Self {
        Self {
            directory,
            max_batches,
            max_bytes,
            presentations: 1,
            reserved: Arc::new(AtomicU64::new(0)),
        }
    }
}

pub(super) struct Capture {
    config: Config,
    file: Option<File>,
    partial: PathBuf,
    complete: PathBuf,
    incomplete: PathBuf,
    bytes: u64,
    batches: u64,
    presentations: u64,
}

impl Capture {
    fn open(config: Config, connection: u64) -> io::Result<Self> {
        let name = format!("gpu-{}-{connection}", std::process::id());
        let partial = config.directory.join(format!("{name}.part"));
        let complete = config.directory.join(format!("{name}.hgpu"));
        let incomplete = config.directory.join(format!("{name}.incomplete"));
        let mut file = File::create(&partial)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        Ok(Self {
            config,
            file: Some(file),
            partial,
            complete,
            incomplete,
            bytes: HEADER_BYTES,
            batches: 0,
            presentations: 0,
        })
    }

    pub(super) fn record(&mut self, batch: &[Cmd], encoded: &[u8]) {
        if self.file.is_none() {
            return;
        }
        let record_bytes = 8_u64.saturating_add(encoded.len() as u64);
        let within_local = self.batches < self.config.max_batches
            && self
                .bytes
                .saturating_add(record_bytes)
                .saturating_add(FOOTER_BYTES)
                <= self.config.max_bytes;
        if !within_local {
            self.finish(false);
            return;
        }
        let within_global = self
            .config
            .reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(record_bytes)
                    .filter(|next| *next <= self.config.max_bytes)
            })
            .is_ok();
        if !within_global {
            self.finish(false);
            return;
        }
        let Some(file) = &mut self.file else {
            return;
        };
        let write = file
            .write_all(&(encoded.len() as u64).to_le_bytes())
            .and_then(|()| file.write_all(encoded));
        if let Err(error) = write {
            hl_log::hl_warn!(
                hl_log::tag::GPU,
                "GPU capture write failed path={} error={error}",
                self.partial.display()
            );
            self.finish(false);
            return;
        }
        self.bytes += record_bytes;
        self.batches += 1;
        self.presentations += batch
            .iter()
            .filter(|command| matches!(command, Cmd::Present { .. }))
            .count() as u64;
        if self.presentations >= self.config.presentations {
            self.finish(true);
        } else if self.batches >= self.config.max_batches {
            self.finish(false);
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.finish(false);
    }

    pub(super) fn record_partial(
        &mut self,
        commands: &[Cmd],
        replayable: bool,
        presentations: usize,
    ) {
        // A normalized residency delta deliberately excludes Present. If one actually succeeded, writing
        // only the persistent commands would produce a syntactically valid trace that silently omits an
        // externally visible effect. Until captures carry presentation outcomes separately, name that
        // limitation honestly by finalizing the trace as nonreplayable.
        if !replayable || presentations != 0 {
            self.invalidate();
            return;
        }
        let encoded = hl_gpu::Encoder::stream(commands);
        self.record(commands, &encoded);
    }

    pub(super) fn active(&self) -> bool {
        self.file.is_some()
    }

    fn finish(&mut self, replayable: bool) {
        let Some(mut file) = self.file.take() else {
            return;
        };
        let marker = if replayable { COMPLETE } else { INCOMPLETE };
        let written = file
            .write_all(marker)
            .and_then(|()| file.sync_all())
            .is_ok();
        drop(file);
        let destination = if replayable && written {
            &self.complete
        } else {
            &self.incomplete
        };
        if let Err(error) = fs::rename(&self.partial, destination) {
            hl_log::hl_warn!(
                hl_log::tag::GPU,
                "GPU capture finalize failed path={} error={error}",
                self.partial.display()
            );
        } else {
            hl_log::hl_info!(
                hl_log::tag::GPU,
                "GPU capture finalized replayable={} batches={} bytes={} path={}",
                replayable && written,
                self.batches,
                self.bytes + FOOTER_BYTES,
                destination.display()
            );
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.finish(false);
    }
}

pub struct Trace;

impl Trace {
    pub fn read(path: &Path) -> io::Result<Vec<Vec<Cmd>>> {
        if fs::metadata(path)?.len() > (1 << 30) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GPU capture exceeds the replay size limit",
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        if bytes.len() < (HEADER_BYTES + FOOTER_BYTES) as usize
            || &bytes[..MAGIC.len()] != MAGIC
            || u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) != VERSION
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid GPU capture header",
            ));
        }
        if &bytes[bytes.len() - COMPLETE.len()..] != COMPLETE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "GPU capture is incomplete",
            ));
        }
        let end = bytes.len() - COMPLETE.len();
        let mut cursor = HEADER_BYTES as usize;
        let mut batches = Vec::new();
        while cursor < end {
            let length_end = cursor
                .checked_add(8)
                .filter(|length_end| *length_end <= end)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "truncated GPU capture record")
                })?;
            let length = u64::from_le_bytes([
                bytes[cursor],
                bytes[cursor + 1],
                bytes[cursor + 2],
                bytes[cursor + 3],
                bytes[cursor + 4],
                bytes[cursor + 5],
                bytes[cursor + 6],
                bytes[cursor + 7],
            ]);
            cursor = length_end;
            let batch_end = cursor
                .checked_add(usize::try_from(length).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "GPU capture record is too large",
                    )
                })?)
                .filter(|batch_end| *batch_end <= end)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "truncated GPU capture batch")
                })?;
            let batch = hl_gpu::Decoder::stream(&bytes[cursor..batch_end])
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            batches.push(batch);
            cursor = batch_end;
        }
        Ok(batches)
    }
}

#[cfg(test)]
impl Trace {
    pub(super) fn write(path: &Path, batches: &[Vec<Cmd>]) -> io::Result<()> {
        let mut file = File::create(path)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        for batch in batches {
            let encoded = hl_gpu::Encoder::stream(batch);
            file.write_all(&(encoded.len() as u64).to_le_bytes())?;
            file.write_all(&encoded)?;
        }
        file.write_all(COMPLETE)
    }
}

#[cfg(test)]
mod tests {
    use hl_gpu::{Cmd, FrameSerial};

    use super::*;

    #[test]
    fn completed_capture_round_trips_canonical_batches() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::testing(root.path().to_owned(), 4, 4096);
        let mut capture = config.open(7).unwrap();
        let batch = vec![Cmd::Present {
            surface: 3,
            texture: 4,
            serial: FrameSerial::new(5).unwrap(),
        }];
        let encoded = hl_gpu::Encoder::stream(&batch);
        assert!(capture.active());
        capture.record(&batch, &encoded);
        assert!(!capture.active());
        drop(capture);

        let path = root
            .path()
            .join(format!("gpu-{}-7.hgpu", std::process::id()));
        assert_eq!(Trace::read(&path).unwrap(), [batch]);
    }

    #[test]
    fn byte_limit_marks_capture_incomplete_and_reader_rejects_it() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::testing(root.path().to_owned(), 4, HEADER_BYTES + FOOTER_BYTES + 8);
        let mut capture = config.open(9).unwrap();
        let batch = vec![Cmd::Present {
            surface: 3,
            texture: 4,
            serial: FrameSerial::new(5).unwrap(),
        }];
        capture.record(&batch, &hl_gpu::Encoder::stream(&batch));
        drop(capture);

        let path = root
            .path()
            .join(format!("gpu-{}-9.incomplete", std::process::id()));
        assert_eq!(
            Trace::read(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn successful_partial_presentation_marks_capture_nonreplayable() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::testing(root.path().to_owned(), 4, 4096);
        let mut capture = config.open(11).unwrap();
        capture.record_partial(&[Cmd::CreateFence(7)], true, 1);
        assert!(!capture.active());
        drop(capture);

        let path = root
            .path()
            .join(format!("gpu-{}-11.incomplete", std::process::id()));
        assert_eq!(
            Trace::read(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn reader_rejects_truncated_completed_record() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("truncated.hgpu");
        let mut bytes = Vec::from(*MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&16_u64.to_le_bytes());
        bytes.extend_from_slice(&[1, 2]);
        bytes.extend_from_slice(COMPLETE);
        fs::write(&path, bytes).unwrap();

        assert_eq!(
            Trace::read(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
