use std::io::{self, Read};

use crate::transport::model::header::SubmitHeader;
use crate::transport::model::readback::{
    ReadbackRequest, READBACK_FAIL, READBACK_MAGIC, READBACK_OK,
};

use super::{Connection, WriteFailure};

#[derive(Debug)]
pub enum ReadbackResponseError {
    Io(io::Error),
    Rejected,
    Malformed(String),
}

impl From<io::Error> for ReadbackResponseError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl Connection<'_> {
    /// Write a device→host request using the reserved readback surface sentinel.
    pub fn write_readback_request(&self, request: &ReadbackRequest) -> io::Result<()> {
        self.write_readback_request_tracked(request)
            .map_err(|failure| failure.error)
    }

    pub fn write_readback_request_tracked(
        &self,
        request: &ReadbackRequest,
    ) -> Result<(), WriteFailure> {
        let payload = request.to_bytes();
        let header = SubmitHeader {
            surface_id: READBACK_MAGIC,
            width: 0,
            height: 0,
            len: payload.len() as u32,
        };
        self.write_frame_tracked(&header, &payload)
    }

    /// Write a status and length-prefixed response without changing its established wire bytes.
    pub fn write_readback_response(&self, status: u8, bytes: &[u8]) -> io::Result<()> {
        let len = u32::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "readback response too large")
        })?;
        let mut header = [0u8; 5];
        header[0] = status;
        header[1..].copy_from_slice(&len.to_le_bytes());
        self.write_full(&header)?;
        self.write_full(bytes)
    }

    /// Validate the response header before allocating its exact expected body.
    pub fn read_readback_response(
        &self,
        expected_len: usize,
    ) -> Result<Vec<u8>, ReadbackResponseError> {
        let expected_len = u32::try_from(expected_len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected readback response is too large",
            )
        })?;
        let mut stream = self.stream;
        let mut status = [0u8; 1];
        stream.read_exact(&mut status)?;
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes)?;
        let len = u32::from_le_bytes(len_bytes);
        match status[0] {
            READBACK_FAIL if len == 0 => Err(ReadbackResponseError::Rejected),
            READBACK_FAIL => Err(ReadbackResponseError::Malformed(
                "failed readback response declared a payload".into(),
            )),
            READBACK_OK if len != expected_len => Err(ReadbackResponseError::Malformed(format!(
                "readback response length {len} does not match requested length {expected_len}"
            ))),
            READBACK_OK => {
                let mut body = vec![0u8; len as usize];
                stream.read_exact(&mut body)?;
                Ok(body)
            }
            _ => Err(ReadbackResponseError::Malformed(format!(
                "invalid readback response status {}",
                status[0]
            ))),
        }
    }
}
