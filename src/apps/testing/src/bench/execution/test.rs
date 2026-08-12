use super::{Benchmark, Capture, DIAGNOSTIC_OUTPUT, capture_size, output_excerpt, parse_phases, stdout_contains};
use crate::suite::BoundedCapture as _;
use hl_container::{Entry, Stream};

fn entry(bytes: usize) -> Entry {
    Entry {
        sequence: 1,
        timestamp_ms: 1,
        stream: Stream::Stdout,
        bytes: vec![0; bytes],
    }
}

#[test]
fn retained_phase_protocol_is_accepted() {
    let phases = parse_phases(b"noise\nPHASE compute us=42 ok=7\n").unwrap();
    assert_eq!(phases, vec![("compute".to_owned(), 42, 7)]);
    assert!(parse_phases(b"PHASE compute ms=42 ok=7\n").is_err());
}

/// A forged timebase leaves every work-counting checksum identical, so only the
/// timebase row can refuse it. Measured on the host: declaring cntfrq=1e9 against a
/// 24MHz counter reported ok=21 and us=2615 for a 100ms sleep, with all seventeen
/// other checksums byte-identical to the correct binary's.
#[test]
fn a_forged_timebase_is_refused_although_every_other_checksum_matches() {
    let honest = b"PHASE timebase us=101848 ok=1\nPHASE compute us=117 ok=441094035400083178\n";
    assert_eq!(parse_phases(honest).unwrap().len(), 2);
    let forged = b"PHASE timebase us=2615 ok=21\nPHASE compute us=2 ok=441094035400083178\n";
    let error = parse_phases(forged).unwrap_err().to_string();
    assert!(error.contains("divergent guest timebase"), "{error}");
    // A guest whose clocks are wrong together still agrees with itself, so only the
    // duration of its own sleep can refuse it.
    let uniform = b"PHASE timebase us=2440 ok=1\n";
    assert!(parse_phases(uniform).unwrap_err().to_string().contains("100ms sleep"));
}

#[test]
fn zero_work_count_is_rejected() {
    assert!(parse_phases(b"PHASE file us=100 ok=0\n").is_err());
}

#[test]
fn provenance_is_typed_and_changes_with_every_identity_field() {
    let baseline = [
        b"provider".as_slice(),
        b"arm64",
        b"artifact",
        b"image",
        b"runner",
        b"definition",
    ];
    let expected = crate::record::FramedIdentity::over(&baseline).unwrap();
    for index in 0..baseline.len() {
        let mut changed = baseline;
        changed[index] = b"changed";
        assert_ne!(
            crate::record::FramedIdentity::over(&changed).unwrap(),
            expected,
            "field {index} did not bind provenance"
        );
    }
    assert_ne!(
        crate::record::FramedIdentity::over(&[b"ab", b"c"]).unwrap(),
        crate::record::FramedIdentity::over(&[b"a", b"bc"]).unwrap(),
    );
}

#[test]
fn combined_definition_accepts_named_phase_rows() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/bench/combined");
    let benchmark = Benchmark::load(&directory, &directory.join("test.yaml")).unwrap();
    let marker = std::fs::read(&benchmark.cases[0].stdout_contains).unwrap();
    let phase = b"PHASE compute us=42 ok=7\n";

    assert!(stdout_contains(phase, &marker));
    assert!(stdout_contains(&[b"noise\n".as_slice(), phase].concat(), &marker));
}

#[test]
fn file_io_definition_exposes_all_typed_phases() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/bench/file_io");
    let benchmark = Benchmark::load(&directory, &directory.join("test.yaml")).unwrap();
    assert_eq!(benchmark.cases.len(), 3);
    let phases =
        parse_phases(b"PHASE scalar_file us=11 ok=1\nPHASE vector_file us=12 ok=2\nPHASE mapped_file us=13 ok=3\n")
            .unwrap();
    assert_eq!(phases.len(), 3);
}

#[test]
fn combined_capture_is_bounded() {
    let within = hl_container::Logs {
        stdout: vec![0; Capture::LIMIT - 1],
        stderr: vec![0],
    };
    assert!(within.bounded().is_ok());
    let over = hl_container::Logs {
        stdout: vec![0; Capture::LIMIT],
        stderr: vec![0],
    };
    assert!(over.bounded().is_err());
}

#[test]
fn incremental_capture_preserves_the_combined_limit() {
    let captured = capture_size(0, &entry(Capture::LIMIT - 1)).unwrap();
    assert_eq!(capture_size(captured, &entry(1)).unwrap(), Capture::LIMIT);
    assert!(capture_size(Capture::LIMIT, &entry(1)).is_err());
    assert!(capture_size(usize::MAX, &entry(1)).is_err());
}

#[test]
fn failure_diagnostic_does_not_repeat_the_full_capture() {
    let excerpt = output_excerpt(&vec![0xff; Capture::LIMIT]);
    assert!(excerpt.len() <= DIAGNOSTIC_OUTPUT);
    assert!(excerpt.contains("truncated"));
}
