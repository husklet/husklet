# `terminate_group`

- [ ] Approved
- Timestamp: `1785820511279697516`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/nested.rs:340:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `terminate_group` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.args`
- `.status`
- `.stderr`
- `.stdout`
- `Command::new`
- `Duration::from_millis`
- `Stdio::null`
- `format!`
- `quiet`
- `thread::sleep`

## Source

````rust
fn terminate_group(process: u32) {
    let group = format!("-{process}");
    let quiet = || Stdio::null();
    let _ = Command::new("kill")
        .args(["-TERM", "--", &group])
        .stdout(quiet())
        .stderr(quiet())
        .status();
    thread::sleep(Duration::from_millis(100));
    let _ = Command::new("kill")
        .args(["-KILL", "--", &group])
        .stdout(quiet())
        .stderr(quiet())
        .status();
}
````

## Related context

### usage in `capture`
fn capture(arguments: &[String], timeout: Duration, limit: usize) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
    let (program, guest) = arguments.split_first().ok_or("empty nested command")?;
    let mut command = Command::new(program);
    command
        .args(guest)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|error| format!("spawn failed: {error}"))?;
    let stdout = child.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.stderr.take().ok_or("missing stderr pipe")?;
    let stdout = thread::spawn(move || drain(stdout, limit));
    let stderr = thread::spawn(move || drain(stderr, limit));
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().map_err(|error| format!("wait failed: {error}"))? {
            Some(status) => break status,
            None if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
            None => {
                timed_out = true;
                terminate_group(child.id());
                let _ = child.kill();
                break child.wait().map_err(|error| format!("reap failed: {error}"))?;
            }
        }
    };
    let stdout = stdout.join().map_err(|_| "stdout capture panicked")??;
    let stderr = stderr.join().map_err(|_| "stderr capture panicked")??;
    if timed_out {
        return Err(format!("timed out after {} seconds", timeout.as_secs()));
    }
    Ok((status.code(), stdout, stderr))
}

`src/apps/testing/src/nested.rs:326:17`

````rust
terminate_group
````
