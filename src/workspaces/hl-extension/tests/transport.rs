//! Frames over a byte stream. Everything a hostile or merely slow peer can do
//! to the arrival of bytes is exercised here: splitting them, concatenating
//! them, lying about a length, and hanging up.

use std::io::{Cursor, Read};
use std::os::unix::net::UnixStream;

use hl_extension::{ChannelId, Frame, Kind, Malformed, Transit, Wire};

fn frame(channel: u32, payload: &[u8]) -> Frame {
    Frame::new(ChannelId::new(channel), Kind::Event, payload.to_vec())
}

/// A reader that yields one byte per call, standing in for a socket that
/// delivers a frame across many small reads.
struct Trickle {
    bytes: Vec<u8>,
    position: usize,
}

impl Read for Trickle {
    fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.bytes.len() || target.is_empty() {
            return Ok(0);
        }
        target[0] = self.bytes[self.position];
        self.position += 1;
        Ok(1)
    }
}

#[test]
fn a_frame_survives_a_round_trip_over_a_stream() {
    let sent = frame(3, b"{\"call\":\"containers.list\"}");
    let mut sender = Wire::new(Vec::new());
    sender.send(&sent).expect("sent");

    let mut receiver = Wire::new(Cursor::new(sender.into_stream()));
    assert_eq!(receiver.receive().expect("received"), sent);
    assert_eq!(receiver.buffered(), 0);
}

#[test]
fn a_frame_split_across_single_byte_reads_still_decodes() {
    let sent = frame(5, b"a payload long enough to span many reads");
    let mut sender = Wire::new(Vec::new());
    sender.send(&sent).expect("sent");

    let mut receiver = Wire::new(Trickle {
        bytes: sender.into_stream(),
        position: 0,
    });
    assert_eq!(receiver.receive().expect("received"), sent);
}

#[test]
fn several_frames_in_one_chunk_decode_in_order() {
    let mut sender = Wire::new(Vec::new());
    let sent: Vec<Frame> = (0..4).map(|index| frame(index, &[index as u8; 7])).collect();
    for one in &sent {
        sender.send(one).expect("sent");
    }

    let mut receiver = Wire::new(Cursor::new(sender.into_stream()));
    let received: Vec<Frame> = (0..sent.len()).map(|_| receiver.receive().expect("received")).collect();
    assert_eq!(received, sent);
}

#[test]
fn a_bogus_oversize_length_is_refused_without_allocating() {
    let mut header = u32::MAX.to_le_bytes().to_vec();
    header.extend_from_slice(&1_u32.to_le_bytes());
    header.extend_from_slice(&[3, 0, 0, 0]);

    let mut receiver = Wire::new(Cursor::new(header));
    let refusal = receiver.receive().expect_err("refused");

    assert_eq!(
        refusal,
        Transit::Malformed(Malformed::Oversize {
            declared: u32::MAX as usize
        })
    );
    assert!(receiver.buffered() <= Frame::HEADER);
}

#[test]
fn a_clean_end_of_stream_is_closed_rather_than_an_error() {
    let mut receiver = Wire::new(Cursor::new(Vec::new()));
    assert_eq!(receiver.receive().expect_err("closed"), Transit::Closed);
}

#[test]
fn a_hangup_partway_through_a_frame_is_closed() {
    let mut sender = Wire::new(Vec::new());
    sender.send(&frame(1, b"truncated by a hangup")).expect("sent");
    let mut bytes = sender.into_stream();
    bytes.truncate(Frame::HEADER + 4);

    let mut receiver = Wire::new(Cursor::new(bytes));
    assert_eq!(receiver.receive().expect_err("closed"), Transit::Closed);
}

#[test]
fn the_buffer_stays_bounded_across_many_frames() {
    let count = 512;
    let mut sender = Wire::new(Vec::new());
    for index in 0..count {
        sender.send(&frame(index, &[7_u8; 64])).expect("sent");
    }
    let bytes = sender.into_stream();
    let total = bytes.len();

    let mut receiver = Wire::new(Cursor::new(bytes));
    let mut high_water = 0;
    for _ in 0..count {
        receiver.receive().expect("received");
        high_water = high_water.max(receiver.buffered());
    }

    assert!(high_water <= Frame::HEADER + Frame::PAYLOAD_LIMIT);
    assert!(high_water < total, "the whole stream was held at once");
    assert_eq!(receiver.buffered(), 0);
}

#[test]
fn frames_cross_a_real_unix_socket_in_order() {
    let (host, extension) = UnixStream::pair().expect("a socket pair");
    let count = 200_u32;

    let writer = std::thread::spawn(move || {
        let mut sender = Wire::new(extension);
        for index in 0..count {
            let payload = format!("frame {index}").into_bytes();
            sender
                .send(&Frame::new(ChannelId::new(index), Kind::Event, payload))
                .expect("sent");
        }
    });

    let mut receiver = Wire::new(host);
    for index in 0..count {
        let received = receiver.receive().expect("received");
        assert_eq!(received.channel, ChannelId::new(index));
        assert_eq!(received.payload, format!("frame {index}").into_bytes());
    }
    writer.join().expect("the writer finished");

    assert_eq!(receiver.receive().expect_err("closed"), Transit::Closed);
    assert_eq!(receiver.buffered(), 0);
}
