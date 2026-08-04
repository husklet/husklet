use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::transport::Upgrade;
use crate::{Error, Result};

/// Terminal dimensions measured in character cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    rows: u16,
    columns: u16,
}

impl Size {
    /// Creates non-empty terminal dimensions.
    ///
    /// # Errors
    /// Returns a protocol error when either dimension is zero.
    pub fn new(rows: u16, columns: u16) -> Result<Self> {
        if rows == 0 || columns == 0 {
            return Err(Error::Protocol("terminal rows and columns must be nonzero".into()));
        }
        Ok(Self { rows, columns })
    }

    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }

    #[must_use]
    pub const fn columns(self) -> u16 {
        self.columns
    }

    pub(super) fn query(self) -> String {
        format!("h={}&w={}", self.rows, self.columns)
    }
}

/// Source of attached process output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Channel {
    Stdout,
    Stderr,
    /// Output merged by a controlling terminal.
    Terminal,
}

/// One ordered output record from an attached process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    channel: Channel,
    bytes: Bytes,
}

impl Output {
    pub(super) fn new(channel: Channel, bytes: Bytes) -> Self {
        Self { channel, bytes }
    }

    #[must_use]
    pub fn channel(&self) -> Channel {
        self.channel
    }

    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Bytes {
        self.bytes
    }
}

/// Bidirectional attachment using either a terminal or separate process pipes.
#[derive(Debug)]
pub enum Session {
    /// Raw, merged controlling-terminal bytes.
    Terminal(Terminal),
    /// Docker's framed stdout and stderr pipes.
    Pipes(Pipes),
}

impl Session {
    pub(crate) fn terminal(stream: Upgrade, limit: usize) -> Self {
        Self::Terminal(Terminal { stream, limit })
    }

    pub(crate) fn pipes(stream: Upgrade, limit: usize) -> Self {
        Self::Pipes(Pipes {
            stream,
            frame_limit: limit,
        })
    }

    /// Write bytes to the process input.
    ///
    /// # Errors
    /// Returns a transport error when the upgraded connection cannot accept the bytes.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Terminal(terminal) => terminal.write(bytes).await,
            Self::Pipes(pipes) => pipes.write(bytes).await,
        }
    }

    /// Close process input while retaining readable output.
    ///
    /// # Errors
    /// Returns a transport error when the connection cannot be shut down cleanly.
    pub async fn close(&mut self) -> Result<()> {
        match self {
            Self::Terminal(terminal) => terminal.close().await,
            Self::Pipes(pipes) => pipes.close().await,
        }
    }

    /// Read the next raw terminal chunk or complete stdout/stderr frame.
    ///
    /// # Errors
    /// Returns a transport or multiplexing protocol error, or a size-limit error.
    pub async fn next(&mut self) -> Result<Option<Output>> {
        match self {
            Self::Terminal(terminal) => terminal.next().await,
            Self::Pipes(pipes) => pipes.next().await,
        }
    }

    /// Split a raw terminal attachment into independently owned input and output halves.
    ///
    /// # Errors
    /// Returns a protocol error when this session uses Docker's framed pipe transport.
    pub fn into_terminal(self) -> Result<(TerminalInput, TerminalOutput)> {
        match self {
            Self::Terminal(terminal) => Ok(terminal.split()),
            Self::Pipes(_) => Err(Error::Protocol(
                "cannot split a framed pipe session as a raw terminal".into(),
            )),
        }
    }
}

/// Raw bidirectional controlling-terminal transport.
#[derive(Debug)]
pub struct Terminal {
    stream: Upgrade,
    limit: usize,
}

impl Terminal {
    fn split(self) -> (TerminalInput, TerminalOutput) {
        let (read, write) = tokio::io::split(self.stream);
        (
            TerminalInput { write },
            TerminalOutput {
                read,
                limit: self.limit,
            },
        )
    }

    /// Write raw bytes to the terminal input.
    ///
    /// # Errors
    /// Returns a transport error when the upgraded connection cannot accept the bytes.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Close terminal input while retaining readable output.
    ///
    /// # Errors
    /// Returns a transport error when the connection cannot be shut down cleanly.
    pub async fn close(&mut self) -> Result<()> {
        self.stream.shutdown().await.map_err(Into::into)
    }

    /// Read the next available raw, merged terminal chunk.
    ///
    /// # Errors
    /// Returns a transport error while reading the upgraded connection.
    pub async fn next(&mut self) -> Result<Option<Output>> {
        let mut bytes = vec![0; self.limit.min(16 * 1024)];
        let count = self.stream.read(&mut bytes).await?;
        if count == 0 {
            return Ok(None);
        }
        bytes.truncate(count);
        Ok(Some(Output::new(Channel::Terminal, Bytes::from(bytes))))
    }
}

/// Independently owned input half of a raw terminal attachment.
#[derive(Debug)]
pub struct TerminalInput {
    write: WriteHalf<Upgrade>,
}

impl TerminalInput {
    /// Write raw bytes and flush them to the terminal.
    ///
    /// # Errors
    /// Returns a transport error when the upgraded connection cannot accept the bytes.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.write.write_all(bytes).await?;
        self.write.flush().await?;
        Ok(())
    }

    /// Close process input without consuming the independently owned output half.
    ///
    /// # Errors
    /// Returns a transport error when the write half cannot be shut down cleanly.
    pub async fn close(&mut self) -> Result<()> {
        self.write.shutdown().await.map_err(Into::into)
    }
}

/// Independently owned output half of a raw terminal attachment.
#[derive(Debug)]
pub struct TerminalOutput {
    read: ReadHalf<Upgrade>,
    limit: usize,
}

impl TerminalOutput {
    /// Read the next available raw terminal chunk.
    ///
    /// # Errors
    /// Returns a transport error while reading the upgraded connection.
    pub async fn next(&mut self) -> Result<Option<Output>> {
        let mut bytes = vec![0; self.limit.min(16 * 1024)];
        let count = self.read.read(&mut bytes).await?;
        if count == 0 {
            return Ok(None);
        }
        bytes.truncate(count);
        Ok(Some(Output::new(Channel::Terminal, Bytes::from(bytes))))
    }
}

/// Docker-multiplexed stdout and stderr pipe transport.
#[derive(Debug)]
pub struct Pipes {
    stream: Upgrade,
    frame_limit: usize,
}

impl Pipes {
    /// Write bytes to standard input.
    ///
    /// # Errors
    /// Returns a transport error when the upgraded connection cannot accept the bytes.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Close standard input while retaining readable output.
    ///
    /// # Errors
    /// Returns a transport error when the connection cannot be shut down cleanly.
    pub async fn close(&mut self) -> Result<()> {
        self.stream.shutdown().await.map_err(Into::into)
    }

    /// Read the next complete Docker stdout or stderr frame.
    ///
    /// # Errors
    /// Returns a transport, framing protocol, or size-limit error.
    pub async fn next(&mut self) -> Result<Option<Output>> {
        let Some(header) = self.header().await? else {
            return Ok(None);
        };
        let channel = match header[0] {
            1 => Channel::Stdout,
            2 => Channel::Stderr,
            value => return Err(Error::Protocol(format!("invalid attach stream identifier {value}"))),
        };
        if header[1..4] != [0, 0, 0] {
            return Err(Error::Protocol("stream frame reserved bytes are not zero".into()));
        }
        let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length > self.frame_limit {
            return Err(Error::ResponseTooLarge {
                limit: self.frame_limit,
            });
        }
        let mut bytes = vec![0; length];
        self.stream.read_exact(&mut bytes).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Protocol("truncated stream frame payload".into())
            } else {
                Error::Transport(error)
            }
        })?;
        Ok(Some(Output::new(channel, Bytes::from(bytes))))
    }

    async fn header(&mut self) -> Result<Option<[u8; 8]>> {
        let mut header = [0; 8];
        if self.stream.read(&mut header[..1]).await? == 0 {
            return Ok(None);
        }
        self.stream.read_exact(&mut header[1..]).await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Protocol("truncated stream frame header".into())
            } else {
                Error::Transport(error)
            }
        })?;
        Ok(Some(header))
    }
}
