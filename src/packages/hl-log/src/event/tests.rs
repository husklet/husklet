use super::*;
use crate::sink::Sink;
use crate::tag;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Collect(Arc<Mutex<Vec<String>>>);

impl Sink for Collect {
    fn write_line(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

fn collector() -> (Arc<Mutex<Vec<String>>>, Box<Collect>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    (lines.clone(), Box::new(Collect(lines)))
}

/// A field keeps its type: a number arrives as a JSON number, not as a quoted string. This is the whole
/// reason the channel exists — a consumer that has to parse `count=100` back out of a sentence is a grep
/// with extra steps.
#[test]
fn fields_keep_their_types() {
    let mut out = String::new();
    Value::from(7u32).write(&mut out);
    out.push(' ');
    Value::from(-3i32).write(&mut out);
    out.push(' ');
    Value::from(true).write(&mut out);
    out.push(' ');
    Value::from("text").write(&mut out);
    assert_eq!(out, r#"7 -3 true "text""#);
}

/// A record must survive a payload that contains the characters records are made of. A driver error with
/// an embedded newline used to split a line in two, and two unparseable halves are worse than a sentence.
#[test]
fn a_hostile_payload_stays_one_parseable_line() {
    let (lines, sink) = collector();
    crate::sink::Events::global().set(sink);
    emit_event(
        tag::GPU.into(),
        Level::Error,
        "submit.refused",
        "m",
        1,
        &[("reason", Value::from("said \"no\"\nline two\tand\\back"))],
    );
    crate::sink::Events::global().reset();

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "one record, whatever the payload holds");
    let line = &captured[0];
    assert_eq!(line.matches('\n').count(), 1, "the only newline is the terminator");
    assert!(line.contains(r#"\"no\""#), "quotes escaped: {line}");
    assert!(line.contains(r"\nline two"), "newline escaped: {line}");
    assert!(line.contains(r"\tand\\back"), "tab and backslash escaped: {line}");
}

/// Non-finite floats have no JSON spelling. Emitting a bare `NaN` produces a record no parser accepts,
/// so the value is preserved as a string rather than the line being lost.
#[test]
fn a_non_finite_float_does_not_break_the_record() {
    let mut out = String::new();
    Value::from(f64::NAN).write(&mut out);
    out.push(' ');
    Value::from(f64::INFINITY).write(&mut out);
    assert_eq!(out, r#""NaN" "inf""#);
}

/// Every record carries its own provenance, and `at` is the field that answers "which of the two call
/// sites did this come from" — the question that voided three conclusions in one session.
#[test]
fn every_record_names_where_it_came_from() {
    let (lines, sink) = collector();
    crate::sink::Events::global().set(sink);
    emit_event(tag::VULKAN.into(), Level::Warn, "thing.happened", "a::b", 42, &[]);
    crate::sink::Events::global().reset();

    let captured = lines.lock().unwrap();
    let line = &captured[0];
    for required in [
        r#""event":"thing.happened""#,
        r#""tag":"vulkan""#,
        r#""level":"warn""#,
        r#""at":"a::b:42""#,
    ] {
        assert!(line.contains(required), "missing {required} in {line}");
    }
    assert!(line.contains("\"ms\":"), "records carry a timestamp: {line}");
    assert!(line.contains("\"thread\":"), "records carry a thread: {line}");
}

/// The global gate and both sinks are process-wide, so these tests take turns. Without this they
/// interleave and each reads another's records — a harness that measures its neighbour.
static SERIAL: Mutex<()> = Mutex::new(());

/// An event is gated exactly like every other macro: a closed tag emits nothing. The structured channel
/// is a second rendering of the same diagnostic, not a second policy for it.
#[test]
fn a_closed_tag_emits_no_event() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (lines, sink) = collector();
    crate::sink::Events::global().set(sink);
    crate::Logging::global().set(crate::Tags::NONE);

    crate::hl_event!(tag::GPU, crate::Level::Error, "should.not.appear", n = 1);

    crate::sink::Events::global().reset();
    assert!(
        lines.lock().unwrap().is_empty(),
        "a closed tag must not emit a record"
    );
}

/// The same call site with the tag open emits one record carrying its fields — the positive control for
/// the test above, which would otherwise pass against a macro that emits nothing ever.
#[test]
fn an_open_tag_emits_the_record() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (lines, sink) = collector();
    crate::sink::Events::global().set(sink);
    crate::Logging::global().set(tag::GPU);
    crate::Logging::global().set_level(crate::Level::Error);

    crate::hl_event!(tag::GPU, crate::Level::Error, "frame.refused", id = 7u32, why = %"no format");

    crate::sink::Events::global().reset();
    crate::Logging::global().set(crate::Tags::NONE);
    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].contains(r#""event":"frame.refused""#), "{}", captured[0]);
    assert!(captured[0].contains(r#""id":7"#), "a number stays a number: {}", captured[0]);
    assert!(captured[0].contains(r#""why":"no format""#), "{}", captured[0]);
}

/// A verdict ignores the tag mask, and that is the whole reason it exists as a separate macro.
///
/// The mask is a subscription: an operator opens a tag to hear a subsystem narrate itself. A refusal is
/// not narration — the operator who did not know to open `gpu` is exactly the one whose frame was
/// dropped. Measured: a run reported that its bundle emitted no presentation heartbeat when the
/// diagnostic was at error level and firing, purely because nobody had opened the tag, and the absence
/// was read as a property of the subject.
///
/// It emits on BOTH channels: the human sentence so a person reading stderr needs no configuration, and
/// the record for whoever is consuming one.
#[test]
fn a_verdict_is_not_maskable_and_reaches_both_channels() {
    let _turn = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let (records, event_sink) = collector();
    let (sentences, human_sink) = collector();
    crate::sink::Events::global().set(event_sink);
    crate::sink::Output::global().set(human_sink);
    // Every tag closed, which silences every other macro in this crate.
    crate::Logging::global().set(crate::Tags::NONE);

    crate::hl_verdict!(tag::WGPU, "encoder_op.refused_in_pass", op = %"ClearRect", pass = 3u32);

    crate::sink::Events::global().reset();
    crate::sink::Output::global().reset();

    let records = records.lock().unwrap();
    assert_eq!(records.len(), 1, "the record is emitted with every tag closed");
    assert!(records[0].contains(r#""event":"encoder_op.refused_in_pass""#), "{}", records[0]);
    assert!(records[0].contains(r#""pass":3"#), "{}", records[0]);

    let sentences = sentences.lock().unwrap();
    assert_eq!(sentences.len(), 1, "and so is the human sentence");
    assert!(sentences[0].contains("encoder_op.refused_in_pass"), "{}", sentences[0]);
    assert!(
        sentences[0].contains("op=ClearRect") && sentences[0].contains("pass=3"),
        "the sentence carries the same fields as the record: {}",
        sentences[0]
    );
}
