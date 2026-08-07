use super::*;

struct Fixture;

impl Fixture {
    fn kind(value: u32) -> SectionKind {
        SectionKind::new(value).unwrap()
    }

    fn writer(limits: ImageLimits) -> CheckpointWriter {
        let mut writer = CheckpointWriter::new(limits);
        writer.push(Section::new(Self::kind(1), 3, b"task".to_vec())).unwrap();
        writer
            .push(Section::new(Self::kind(7), 2, (0_u8..=127).collect()))
            .unwrap();
        writer
    }

    fn bytes(limits: ImageLimits) -> Vec<u8> {
        let writer = Self::writer(limits);
        let mut sink = MemorySink::new();
        writer.publish(&mut sink).unwrap();
        sink.committed().unwrap().to_vec()
    }
}

#[test]
fn image_round_trip() {
    let limits = ImageLimits::default();
    let bytes = Fixture::bytes(limits);
    let mut source = MemorySource::new(bytes);
    source.set_chunk_size(3);
    let image = CheckpointReader::new(limits).read(&mut source).unwrap();

    assert_eq!(image.sections().len(), 2);
    assert_eq!(image.section(Fixture::kind(1)).unwrap().version(), 3);
    assert_eq!(image.section(Fixture::kind(1)).unwrap().bytes(), b"task");
    assert_eq!(image.section(Fixture::kind(7)).unwrap().bytes().len(), 128);
    assert!(image.section(Fixture::kind(2)).is_none());
}

#[test]
fn partial_sink_and() {
    let limits = ImageLimits::default();
    let writer = Fixture::writer(limits);
    let mut sink = MemorySink::new();
    sink.set_chunk_size(1);
    writer.publish(&mut sink).unwrap();

    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    source.set_chunk_size(1);
    assert_eq!(
        CheckpointReader::new(limits)
            .read(&mut source)
            .unwrap()
            .sections()
            .len(),
        2
    );
}

#[test]
fn interruption_and_backpressure() {
    let limits = ImageLimits::default();
    for error in [PortError::Interrupted, PortError::WouldBlock] {
        let mut sink = MemorySink::with_fault(Fault { operation: 2, error });
        Fixture::writer(limits).publish(&mut sink).unwrap();
        assert!(sink.committed().is_some());
    }

    let bytes = Fixture::bytes(limits);
    for error in [PortError::Interrupted, PortError::WouldBlock] {
        let mut source = MemorySource::with_fault(bytes.clone(), Fault { operation: 2, error });
        assert!(CheckpointReader::new(limits).read(&mut source).is_ok());
    }
}

#[test]
fn cancellation_aborts_without() {
    let limits = ImageLimits::default();
    let writer = Fixture::writer(limits);
    for operation in 1..12 {
        let mut sink = MemorySink::with_fault(Fault {
            operation,
            error: PortError::Canceled,
        });
        sink.set_chunk_size(32);
        let result = writer.publish(&mut sink);
        if result.is_ok() {
            break;
        }
        assert!(matches!(result, Err(ImageError::Port(PortError::Canceled))));
        assert!(sink.committed().is_none());
    }

    let bytes = Fixture::bytes(limits);
    let mut source = MemorySource::with_fault(
        bytes,
        Fault {
            operation: 2,
            error: PortError::Canceled,
        },
    );
    assert!(matches!(
        CheckpointReader::new(limits).read(&mut source),
        Err(ImageError::Port(PortError::Canceled))
    ));
}

#[test]
fn failed_replacement_preserves() {
    let limits = ImageLimits::default();
    let writer = Fixture::writer(limits);
    let mut sink = MemorySink::new();
    writer.publish(&mut sink).unwrap();
    let previous = sink.committed().unwrap().to_vec();

    sink.inject(Fault {
        operation: 3,
        error: PortError::Failed,
    });
    assert!(matches!(
        writer.publish(&mut sink),
        Err(ImageError::Port(PortError::Failed))
    ));
    assert_eq!(sink.committed(), Some(previous.as_slice()));
}

#[test]
fn every_truncation_is() {
    let limits = ImageLimits::default();
    let bytes = Fixture::bytes(limits);
    for length in 0..bytes.len() {
        let mut source = MemorySource::new(bytes[..length].to_vec());
        assert!(
            CheckpointReader::new(limits).read(&mut source).is_err(),
            "accepted truncation at {length}"
        );
    }
}

#[test]
fn corruption_in_each() {
    let limits = ImageLimits::default();
    let original = Fixture::bytes(limits);
    for offset in 0..original.len() {
        let mut corrupt = original.clone();
        corrupt[offset] ^= 0x80;
        let mut source = MemorySource::new(corrupt);
        assert!(
            CheckpointReader::new(limits).read(&mut source).is_err(),
            "accepted corruption at {offset}"
        );
    }
}

#[test]
fn section_count_size() {
    assert!(SectionKind::new(0).is_err());

    let limits = ImageLimits::new(1, 4, 91);
    let mut writer = CheckpointWriter::new(limits);
    writer.push(Section::new(Fixture::kind(2), 1, vec![0; 4])).unwrap();
    assert!(matches!(
        writer.push(Section::new(Fixture::kind(3), 1, Vec::new())),
        Err(ImageError::SectionLimit)
    ));

    let mut oversized = CheckpointWriter::new(limits);
    assert!(matches!(
        oversized.push(Section::new(Fixture::kind(1), 1, vec![0; 5])),
        Err(ImageError::SectionTooLarge { length: 5, maximum: 4 })
    ));

    let mut unordered = CheckpointWriter::new(ImageLimits::default());
    unordered.push(Section::new(Fixture::kind(3), 1, Vec::new())).unwrap();
    assert!(matches!(
        unordered.push(Section::new(Fixture::kind(3), 2, Vec::new())),
        Err(ImageError::DuplicateOrUnorderedSection)
    ));

    let mut sink = MemorySink::new();
    assert!(matches!(writer.publish(&mut sink), Err(ImageError::ImageTooLarge { .. })));
    assert!(sink.committed().is_none());
}

#[test]
fn reader_rejects_images() {
    let bytes = Fixture::bytes(ImageLimits::default());
    let limits = ImageLimits::new(256, 4096, bytes.len() - 1);
    let mut source = MemorySource::new(bytes);
    assert!(matches!(
        CheckpointReader::new(limits).read(&mut source),
        Err(ImageError::ImageTooLarge { .. })
    ));
}

struct ZeroSink;

impl CheckpointSink for ZeroSink {
    fn begin(&mut self, _image_size: usize) -> Result<(), PortError> {
        Ok(())
    }

    fn write(&mut self, _bytes: &[u8]) -> Result<usize, PortError> {
        Ok(0)
    }

    fn wait_writable(&mut self) -> Result<(), PortError> {
        Ok(())
    }

    fn commit(&mut self) -> Result<(), PortError> {
        panic!("zero-progress image must not commit")
    }

    fn abort(&mut self) {}
}

#[test]
fn zero_progress_is() {
    assert!(matches!(
        Fixture::writer(ImageLimits::default()).publish(&mut ZeroSink),
        Err(ImageError::ZeroProgress)
    ));
}

struct ShortSizeSource {
    bytes: Vec<u8>,
    offset: usize,
}

impl CheckpointSource for ShortSizeSource {
    fn image_size(&mut self) -> Result<usize, PortError> {
        Ok(self.bytes.len() - 1)
    }

    fn read(&mut self, output: &mut [u8]) -> Result<usize, PortError> {
        let count = output.len().min(self.bytes.len().saturating_sub(self.offset));
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }

    fn wait_readable(&mut self) -> Result<(), PortError> {
        Ok(())
    }
}

#[test]
fn source_size_underreport() {
    let mut source = ShortSizeSource {
        bytes: Fixture::bytes(ImageLimits::default()),
        offset: 0,
    };
    assert!(matches!(
        CheckpointReader::new(ImageLimits::default()).read(&mut source),
        Err(ImageError::TrailingBytes)
    ));
}
