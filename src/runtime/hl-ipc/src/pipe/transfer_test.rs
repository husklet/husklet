use std::sync::{Arc, Barrier};
use std::thread;

use hl_descriptor::{ObjectError, OpenFileDescription};

use crate::{PIPE_BUF, Pipe, PipeTransfer, PipeTransferMode};

fn transfer(
    source: &crate::PipeEndpoint,
    target: &crate::PipeEndpoint,
    maximum: usize,
    mode: PipeTransferMode,
) -> Result<usize, ObjectError> {
    PipeTransfer::execute(
        source.pipe_transfer_endpoint().unwrap(),
        target.pipe_transfer_endpoint().unwrap(),
        maximum,
        mode,
        true,
        None,
    )
}

#[test]
fn tee_duplicates_without() {
    let source = Pipe::new(true);
    let target = Pipe::new(true);
    source.writer.write(b"teedata").unwrap();
    assert_eq!(
        transfer(&source.reader, &target.writer, 7, PipeTransferMode::Duplicate),
        Ok(7)
    );
    let mut original = [0_u8; 7];
    let mut duplicate = [0_u8; 7];
    assert_eq!(source.reader.read(&mut original), Ok(7));
    assert_eq!(target.reader.read(&mut duplicate), Ok(7));
    assert_eq!(&original, b"teedata");
    assert_eq!(&duplicate, b"teedata");

    source.writer.write(b"move").unwrap();
    assert_eq!(
        transfer(&source.reader, &target.writer, 4, PipeTransferMode::Move),
        Ok(4)
    );
    assert_eq!(source.reader.read(&mut original), Err(ObjectError::WouldBlock));
    assert_eq!(target.reader.read(&mut duplicate), Ok(4));
    assert_eq!(&duplicate[..4], b"move");
}

#[test]
fn transfer_respects_capacity() {
    let source = Pipe::with_capacity(PIPE_BUF, true).unwrap();
    let target = Pipe::with_capacity(PIPE_BUF, true).unwrap();
    source.writer.write(&vec![1; PIPE_BUF]).unwrap();
    target.writer.write(&vec![2; PIPE_BUF - 2]).unwrap();
    assert_eq!(
        transfer(&source.reader, &target.writer, PIPE_BUF, PipeTransferMode::Move),
        Ok(2)
    );
    target.reader.close();
    assert_eq!(
        transfer(&source.reader, &target.writer, 1, PipeTransferMode::Move),
        Err(ObjectError::BrokenPipe)
    );
    assert_eq!(
        transfer(&source.reader, &source.writer, 1, PipeTransferMode::Duplicate),
        Err(ObjectError::InvalidArgument)
    );
    source.writer.close();
    let mut drain = vec![0; PIPE_BUF];
    source.reader.read(&mut drain).unwrap();
    assert_eq!(
        transfer(&source.reader, &Pipe::new(true).writer, 1, PipeTransferMode::Move),
        Ok(0)
    );
}

#[test]
fn opposite_overlapping_transfers() {
    let left = Arc::new(Pipe::new(true));
    let right = Arc::new(Pipe::new(true));
    left.writer.write(b"left").unwrap();
    right.writer.write(b"right").unwrap();
    let start = Arc::new(Barrier::new(3));
    let left_to_right = {
        let left = left.clone();
        let right = right.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            transfer(&left.reader, &right.writer, 4, PipeTransferMode::Move)
        })
    };
    let right_to_left = {
        let left = left.clone();
        let right = right.clone();
        let start = start.clone();
        thread::spawn(move || {
            start.wait();
            transfer(&right.reader, &left.writer, 5, PipeTransferMode::Move)
        })
    };
    start.wait();
    assert_eq!(left_to_right.join().unwrap(), Ok(4));
    assert_eq!(right_to_left.join().unwrap(), Ok(5));
}

#[test]
fn packet_transfer_retains() {
    let source = Pipe::new_packet(true);
    let target = Pipe::new_packet(true);
    source.writer.write(b"packet").unwrap();
    assert_eq!(
        transfer(&source.reader, &target.writer, 6, PipeTransferMode::Duplicate),
        Ok(6)
    );
    let mut short = [0_u8; 3];
    assert_eq!(target.reader.read(&mut short), Ok(3));
    assert_eq!(&short, b"pac");
    let mut empty = [0_u8; 1];
    assert_eq!(target.reader.read(&mut empty), Err(ObjectError::WouldBlock));
}
