/// Decoded Docker multiplexed stdout/stderr response.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContainerLogs {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Selection applied to Docker container log replay and following.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogOptions {
    pub follow: bool,
    pub streams: LogStreams,
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub timestamps: bool,
    pub tail: Option<usize>,
}

/// Output streams selected for Docker log replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogStreams {
    pub stdout: bool,
    pub stderr: bool,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            follow: false,
            streams: LogStreams {
                stdout: true,
                stderr: true,
            },
            since_ms: None,
            until_ms: None,
            timestamps: false,
            tail: None,
        }
    }
}

#[cfg(feature = "runtime")]
impl LogOptions {
    pub(crate) fn accepts(&self, entry: &hl_container::Entry) -> bool {
        let stream = match entry.stream {
            hl_container::Stream::Stdout => self.streams.stdout,
            hl_container::Stream::Stderr => self.streams.stderr,
        };
        stream
            && self.since_ms.is_none_or(|since| entry.timestamp_ms >= since)
            && self.until_ms.is_none_or(|until| entry.timestamp_ms <= until)
    }

    pub(crate) fn replay(&self, mut entries: Vec<hl_container::Entry>) -> Vec<hl_container::Entry> {
        entries.retain(|entry| self.accepts(entry));
        let Some(tail) = self.tail else {
            return entries;
        };
        let mut lines = Vec::new();
        for entry in entries {
            lines.extend(Self::lines(&entry));
        }
        let remove = lines.len().saturating_sub(tail);
        lines.drain(..remove);
        lines
    }

    fn lines(entry: &hl_container::Entry) -> Vec<hl_container::Entry> {
        let mut lines = Vec::new();
        let mut start = 0;
        for (index, byte) in entry.bytes.iter().enumerate() {
            if *byte == b'\n' {
                let mut line = entry.clone();
                line.bytes = entry.bytes[start..=index].to_vec();
                lines.push(line);
                start = index + 1;
            }
        }
        if start < entry.bytes.len() {
            let mut line = entry.clone();
            line.bytes = entry.bytes[start..].to_vec();
            lines.push(line);
        }
        lines
    }
}

#[cfg(feature = "runtime")]
pub(crate) struct LogEncoder {
    timestamps: bool,
    terminal: bool,
    starts: [bool; 2],
}

#[cfg(feature = "runtime")]
impl LogEncoder {
    pub(crate) fn new(timestamps: bool, terminal: bool) -> Self {
        Self {
            timestamps,
            terminal,
            starts: [true; 2],
        }
    }

    pub(crate) fn frame(&mut self, entry: &hl_container::Entry) -> Vec<u8> {
        if !self.timestamps {
            return if self.terminal {
                entry.bytes.clone()
            } else {
                ContainerLogs::frame(entry)
            };
        }
        let index = match entry.stream {
            hl_container::Stream::Stdout => 0,
            hl_container::Stream::Stderr => 1,
        };
        let start = &mut self.starts[index];
        let timestamp = chrono::DateTime::from_timestamp_millis(i64::try_from(entry.timestamp_ms).unwrap_or(i64::MAX))
            .unwrap_or(chrono::DateTime::UNIX_EPOCH)
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        let mut payload = Vec::with_capacity(entry.bytes.len().saturating_add(timestamp.len() + 1));
        for byte in &entry.bytes {
            if *start {
                payload.extend_from_slice(timestamp.as_bytes());
                payload.push(b' ');
                *start = false;
            }
            payload.push(*byte);
            if *byte == b'\n' {
                *start = true;
            }
        }
        let mut entry = entry.clone();
        entry.bytes = payload;
        if self.terminal {
            entry.bytes
        } else {
            ContainerLogs::frame(&entry)
        }
    }
}

/// Invalid Docker raw-stream framing.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid Docker log stream: {0}")]
pub struct LogProtocolError(String);

impl ContainerLogs {
    #[must_use]
    pub fn encode(stdout: &[u8], stderr: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(stdout.len() + stderr.len() + 16);
        append_frame(&mut output, 1, stdout);
        append_frame(&mut output, 2, stderr);
        output
    }

    #[cfg(feature = "runtime")]
    #[must_use]
    pub(crate) fn frame(entry: &hl_container::Entry) -> Vec<u8> {
        let stream = match entry.stream {
            hl_container::Stream::Stdout => 1,
            hl_container::Stream::Stderr => 2,
        };
        let mut output = Vec::with_capacity(entry.bytes.len().saturating_add(8));
        append_frame(&mut output, stream, &entry.bytes);
        output
    }

    /// Decode Docker's eight-byte multiplex headers and concatenate frames by stream.
    ///
    /// # Errors
    /// Returns an error for unknown streams, nonzero reserved bytes, oversized lengths, or truncation.
    pub fn decode(mut bytes: &[u8]) -> Result<Self, LogProtocolError> {
        let mut logs = Self::default();
        while !bytes.is_empty() {
            if bytes.len() < 8 {
                return Err(LogProtocolError("truncated frame header".into()));
            }
            if bytes[1..4] != [0, 0, 0] {
                return Err(LogProtocolError("reserved header bytes are nonzero".into()));
            }
            let stream = bytes[0];
            let size = usize::try_from(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
                .map_err(|_| LogProtocolError("frame length exceeds address space".into()))?;
            bytes = &bytes[8..];
            if bytes.len() < size {
                return Err(LogProtocolError("truncated frame payload".into()));
            }
            let (payload, remaining) = bytes.split_at(size);
            match stream {
                1 => logs.stdout.extend_from_slice(payload),
                2 => logs.stderr.extend_from_slice(payload),
                value => return Err(LogProtocolError(format!("unknown stream {value}"))),
            }
            bytes = remaining;
        }
        Ok(logs)
    }
}

fn append_frame(output: &mut Vec<u8>, stream: u8, payload: &[u8]) {
    if payload.is_empty() {
        output.push(stream);
        output.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
        return;
    }
    let maximum = usize::try_from(u32::MAX).unwrap_or(usize::MAX);
    for chunk in payload.chunks(maximum) {
        let size = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
        output.push(stream);
        output.extend_from_slice(&[0, 0, 0]);
        output.extend_from_slice(&size.to_be_bytes());
        output.extend_from_slice(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::{ContainerLogs, LogEncoder, LogOptions, LogProtocolError};
    #[test]
    fn multiplexed_logs_round_trip_binary_streams() {
        let encoded = ContainerLogs::encode(b"out\0", b"err\xff");
        let decoded = ContainerLogs::decode(&encoded).unwrap();
        assert_eq!(decoded.stdout, b"out\0");
        assert_eq!(decoded.stderr, b"err\xff");
    }

    #[test]
    fn multiplexed_log_frames_have_docker_headers_including_empty_payloads() {
        assert_eq!(
            ContainerLogs::encode(b"hello", b""),
            [
                &[1, 0, 0, 0, 0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'][..],
                &[2, 0, 0, 0, 0, 0, 0, 0][..],
            ]
            .concat()
        );
        assert_eq!(
            ContainerLogs::decode(&[2, 0, 0, 0, 0, 0, 0, 3, b'e', b'r', b'r'])
                .unwrap()
                .stderr,
            b"err"
        );
    }
    #[test]
    fn multiplexed_logs_reject_truncated_and_unknown_frames() {
        assert!(matches!(ContainerLogs::decode(&[1, 0, 0]), Err(LogProtocolError(_))));
        assert!(ContainerLogs::decode(&[9, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(ContainerLogs::decode(&[1, 0, 0, 0, 0, 0, 0, 2, 1]).is_err());
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn timestamp_encoder_prefixes_each_line_and_preserves_stream_framing() {
        let mut encoder = LogEncoder::new(true, false);
        let first = hl_container::Entry {
            sequence: 1,
            timestamp_ms: 0,
            stream: hl_container::Stream::Stdout,
            bytes: b"one\ntwo".to_vec(),
        };
        let second = hl_container::Entry {
            sequence: 2,
            timestamp_ms: 1_000,
            stream: hl_container::Stream::Stdout,
            bytes: b"-continued\n".to_vec(),
        };
        let decoded = ContainerLogs::decode(&[encoder.frame(&first), encoder.frame(&second)].concat()).unwrap();
        assert_eq!(
            decoded.stdout,
            b"1970-01-01T00:00:00.000000000Z one\n1970-01-01T00:00:00.000000000Z two-continued\n"
        );
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn terminal_logs_are_raw_with_linewise_timestamps() {
        let entry = hl_container::Entry {
            sequence: 1,
            timestamp_ms: 0,
            stream: hl_container::Stream::Stdout,
            bytes: b"one\ntwo\n".to_vec(),
        };
        assert_eq!(LogEncoder::new(false, true).frame(&entry), b"one\ntwo\n");
        assert_eq!(
            LogEncoder::new(true, true).frame(&entry),
            b"1970-01-01T00:00:00.000000000Z one\n1970-01-01T00:00:00.000000000Z two\n"
        );
    }
    #[cfg(feature = "runtime")]
    #[test]
    fn log_tail_counts_lines_instead_of_reader_chunks() {
        let entry = hl_container::Entry {
            sequence: 1,
            timestamp_ms: 1,
            stream: hl_container::Stream::Stdout,
            bytes: b"one\ntwo\nthree\n".to_vec(),
        };
        let lines = LogOptions {
            tail: Some(2),
            ..Default::default()
        }
        .replay(vec![entry]);
        assert_eq!(
            lines.into_iter().flat_map(|entry| entry.bytes).collect::<Vec<_>>(),
            b"two\nthree\n"
        );
    }
}
