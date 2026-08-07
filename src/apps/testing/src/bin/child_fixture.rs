// The fixture deliberately leaves the child running; the parent under test reaps it.
#![allow(clippy::zombie_processes)]

//! Purpose-built native-launch fixture with no engine behavior.

use std::io::Write;

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let activation = std::env::var("HL_ACTIVATION_FD").unwrap_or_default();
    let process_group = Fixture::process_group().unwrap_or_default();
    let descriptors = Fixture::descriptors().join(",");
    println!(
        "argv={}\nactivation={activation}\npgrp={process_group}\nfds={descriptors}",
        arguments.join("|")
    );
    std::io::stdout().flush().expect("fixture stdout");
    if let Some(path) = std::env::var_os("HL_FIXTURE_DESCENDANT")
        && std::env::var_os("HL_FIXTURE_CHILD").is_none()
    {
        let escape = std::env::var_os("HL_FIXTURE_ESCAPE").is_some();
        hl_engine::native::ChildFixture::spawn(std::path::Path::new(&path), escape).expect("fixture descendant");
    }
    if std::env::var("HL_FIXTURE_ESCAPE").as_deref() == Ok("1") && std::env::var_os("HL_FIXTURE_CHILD").is_some() {
        hl_engine::native::ChildFixture::detach().expect("fixture session");
    }
    if std::env::var_os("HL_FIXTURE_BLOCK").is_some() {
        loop {
            std::thread::park_timeout(std::time::Duration::from_secs(60));
        }
    }
    let code = std::env::var("HL_FIXTURE_EXIT")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    std::process::exit(code);
}

struct Fixture;

impl Fixture {
    fn process_group() -> Option<String> {
        let status = std::fs::read_to_string("/proc/self/stat").ok()?;
        let (_, fields) = status.rsplit_once(") ")?;
        fields.split_whitespace().nth(2).map(str::to_owned)
    }

    fn descriptors() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        let mut descriptors: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let descriptor = entry.file_name().into_string().ok()?;
                let target = std::fs::read_link(entry.path()).ok()?;
                Some(format!("{descriptor}={}", target.display()))
            })
            .collect();
        descriptors.sort();
        descriptors
    }
}
