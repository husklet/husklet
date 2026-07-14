//! A single-producer/single-consumer byte ring — the in-memory model of the shared-memory command
//! ring the guest producer writes and the host executor drains (the batching/coalescing transport).
//!
//! On the real host this lives in a POSIX-shm region shared guest↔`hl-display`, with the head/tail as
//! atomics and a futex/eventfd doorbell (gfxstream's model). Here it is a plain in-process ring so the
//! framing + batching logic is unit-testable headless. Frames are length-prefixed (u32) blobs — one
//! encoded [`crate::ir::Cmd`] per frame via [`crate::ir::Cmd::frame`], or any opaque batch.

/// A bounded byte ring. `push_frame` writes a length-prefixed frame; `pop_frame` reads one back. Not
/// thread-safe here (the shm version uses atomics); this models the wire discipline + backpressure.
pub struct CommandRing {
    buf: Vec<u8>,
    head: usize, // read cursor
    tail: usize, // write cursor
    len: usize,  // bytes currently queued
}

impl CommandRing {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: vec![0u8; cap.max(8)],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }
    pub fn used(&self) -> usize {
        self.len
    }
    pub fn free(&self) -> usize {
        self.buf.len() - self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn write_bytes(&mut self, src: &[u8]) {
        let cap = self.buf.len();
        for &b in src {
            self.buf[self.tail] = b;
            self.tail = (self.tail + 1) % cap;
        }
        self.len += src.len();
    }

    fn read_bytes(&mut self, n: usize) -> Vec<u8> {
        let cap = self.buf.len();
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.buf[self.head]);
            self.head = (self.head + 1) % cap;
        }
        self.len -= n;
        out
    }

    /// Enqueue one frame (4-byte LE length prefix + body). Returns `false` (backpressure) if it doesn't
    /// fit — the producer must wait for the consumer to drain, exactly like the real ring.
    pub fn push_frame(&mut self, body: &[u8]) -> bool {
        let need = 4 + body.len();
        if need > self.free() {
            return false;
        }
        self.write_bytes(&(body.len() as u32).to_le_bytes());
        self.write_bytes(body);
        true
    }

    /// Peek the length prefix of the frame at `head` without consuming any bytes.
    fn peek_len(&self) -> Option<usize> {
        if self.len < 4 {
            return None;
        }
        let cap = self.buf.len();
        let mut lb = [0u8; 4];
        for (i, b) in lb.iter_mut().enumerate() {
            *b = self.buf[(self.head + i) % cap];
        }
        Some(u32::from_le_bytes(lb) as usize)
    }

    /// Dequeue one complete frame body, or `None` if the ring is empty **or** only holds a partial
    /// frame. A partial frame (header present but body not yet fully written) stays buffered until the
    /// producer supplies the rest — the reader never consumes the header early or fabricates body bytes.
    pub fn pop_frame(&mut self) -> Option<Vec<u8>> {
        let n = self.peek_len()?;
        // The full frame is `4 + n` bytes; if the ring doesn't hold all of it yet, leave it buffered.
        if self.len < 4 + n {
            return None;
        }
        let _ = self.read_bytes(4);
        Some(self.read_bytes(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_frame_waits_for_complete_body() {
        // Model a torn write: the producer has written the 4-byte length header and only part of the
        // body. The reader must NOT consume the header or fabricate the missing bytes — it leaves the
        // partial frame buffered and returns None until the whole body arrives.
        let mut ring = CommandRing::with_capacity(64);
        ring.write_bytes(&(6u32).to_le_bytes()); // header: body is 6 bytes
        ring.write_bytes(&[1, 2, 3]); // only 3 of 6 body bytes so far
        assert_eq!(ring.pop_frame(), None, "partial frame must not be popped");
        assert_eq!(ring.used(), 7, "partial frame stays buffered intact");
        ring.write_bytes(&[4, 5, 6]); // rest of the body
        assert_eq!(ring.pop_frame(), Some(vec![1, 2, 3, 4, 5, 6]));
        assert!(ring.is_empty());
    }
}
