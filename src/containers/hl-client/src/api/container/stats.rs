use crate::model::Stats;
use crate::transport::Stream;
use crate::{Error, Result};
use bytes::{Buf, BytesMut};

/// Pull-based stream of complete Docker resource samples.
#[derive(Debug)]
pub struct StatsStream {
    stream: Stream,
    buffer: BytesMut,
    limit: usize,
    ended: bool,
}

impl StatsStream {
    pub(super) fn new(stream: Stream) -> Self {
        let limit = stream.limit();
        Self {
            stream,
            buffer: BytesMut::new(),
            limit,
            ended: false,
        }
    }

    fn line_end(&self) -> Option<usize> {
        self.buffer.iter().position(|byte| *byte == b'\n')
    }

    fn take(&mut self, end: usize) -> Result<Stats> {
        let value = serde_json::from_slice(&self.buffer[..end])?;
        self.buffer.advance(end + 1);
        Ok(value)
    }

    fn finish(&mut self) -> Result<Option<Stats>> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        let value = serde_json::from_slice(&self.buffer)?;
        self.buffer.clear();
        Ok(Some(value))
    }

    /// Read one complete resource sample independently of HTTP frame boundaries.
    ///
    /// # Errors
    /// Returns transport, malformed JSON, truncated-record, or response-limit failures.
    pub async fn next(&mut self) -> Result<Option<Stats>> {
        loop {
            if let Some(index) = self.line_end() {
                return self.take(index).map(Some);
            }
            if self.ended {
                return self.finish();
            }
            match self.stream.next_chunk().await? {
                Some(chunk) if self.buffer.len().saturating_add(chunk.len()) <= self.limit => {
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(_) => return Err(Error::ResponseTooLarge { limit: self.limit }),
                None => self.ended = true,
            }
        }
    }
}
