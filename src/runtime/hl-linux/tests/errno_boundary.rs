//! The errno conversion boundary is observable: opening a domain's tag names the typed
//! domain error that produced the guest's errno.

use std::sync::{Arc, Mutex};

struct Collector(Arc<Mutex<Vec<String>>>);

impl hl_log::Sink for Collector {
    fn write_line(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

#[test]
fn a_domain_error_names_itself_and_its_errno() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    hl_log::sink::Output::global().set(Box::new(Collector(Arc::clone(&lines))));
    hl_log::Config {
        logging: hl_log::tag::SYSCALL | hl_log::tag::TASK,
        level: hl_log::Level::Debug,
        profiling: hl_log::tag::NONE,
    }
    .apply();

    assert_eq!(hl_linux::MarshalError::Invalid.errno(), hl_linux::Errno::EINVAL);
    assert_eq!(
        hl_linux::ProcessMarshalError::NameTooLong.errno(),
        hl_linux::Errno::ENAMETOOLONG
    );

    hl_log::Config::default().apply();
    hl_log::sink::Output::global().reset();

    let captured = lines.lock().unwrap().join("");
    if hl_log::VERBOSE_COMPILED {
        assert!(
            captured.contains("guest marshalling error mapped error=Invalid errno=22"),
            "marshal conversion unobservable: {captured}"
        );
        assert!(
            captured.contains("process abi error mapped error=NameTooLong errno=36"),
            "process conversion unobservable: {captured}"
        );
    }
}
