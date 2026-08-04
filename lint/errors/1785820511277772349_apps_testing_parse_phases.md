# `parse_phases`

- [ ] Approved
- Timestamp: `1785820511277772349`
- Domain: `apps`
- Package: `testing`
- Rule: `unclassified-free-function`
- Severity: `error`
- Source: `src/apps/testing/src/bench/execution.rs:258:1`
- Queue: `unclassified`
- Arguments: `1`
- Classification: `unclassified`
- Usage resolution: `unique name in scanned tree`

## Finding

unclassified free function `parse_phases` has 1 argument

Help: refactor it or add a temporary #[hl_design::classify(...)] classification

## Review

- Does one argument already have a meaningful receiver type?
- Do related functions share this value and its invariants?
- Would a wrapper collect cohesive behavior, or only hide one helper?
- Is this a complete low-level algorithm that should remain free?

## Decision


## Dependencies

- `.and_then`
- `.collect`
- `.filter`
- `.lines`
- `.map`
- `.map_err`
- `.next`
- `.ok_or_else`
- `.parse`
- `.split_whitespace`
- `.starts_with`
- `.strip_prefix`
- `.to_owned`
- `Ok`
- `format!`
- `std::str::from_utf8`

## Source

````rust
fn parse_phases(stdout: &[u8]) -> std::result::Result<Vec<(String, u128, u64)>, Error> {
    let text = std::str::from_utf8(stdout).map_err(|_| "benchmark stdout is not UTF-8")?;
    text.lines()
        .filter(|line| line.starts_with("PHASE "))
        .map(|line| {
            let mut fields = line.split_whitespace();
            let _protocol = fields.next();
            let name = fields.next().ok_or_else(|| format!("invalid PHASE row {line:?}"))?;
            let time = fields
                .next()
                .and_then(|field| field.strip_prefix("us="))
                .ok_or_else(|| format!("invalid PHASE time {line:?}"))?
                .parse::<u128>()?;
            let checksum = fields
                .next()
                .and_then(|field| field.strip_prefix("ok="))
                .ok_or_else(|| format!("invalid PHASE checksum {line:?}"))?
                .parse::<u64>()?;
            Ok((name.to_owned(), time, checksum))
        })
        .collect()
}
````

## Related context

### usage in `execute`
async fn execute(&self, expected_stdout: &[u8]) -> std::result::Result<(u128, Vec<(String, u128, u64)>), Error> {
        let started = Instant::now();
        self.containers.create(self.spec.clone()).await?;
        self.containers.start(&self.name).await?;
        let status = tokio::time::timeout(Duration::from_secs(self.case.timeout), self.containers.wait(&self.name))
            .await
            .map_err(|_| format!("timed out after {} seconds", self.case.timeout))??;
        let elapsed = started.elapsed().as_millis();
        let logs = self.containers.logs(&self.name).await?;
        let captured = logs.stdout.len().saturating_add(logs.stderr.len());
        if captured > CAPTURE_LIMIT {
            return Err(format!("output exceeded {CAPTURE_LIMIT} bytes").into());
        }
        if status != ExitStatus::Code(self.case.exit) {
            return Err(format!("exit {status:?}, expected {}", self.case.exit).into());
        }
        if expected_stdout.is_empty() && !logs.stdout.is_empty() {
            return Err(format!("expected empty stdout; stdout={:?}", logs.stdout).into());
        }
        if !expected_stdout.is_empty()
            && !logs
                .stdout
                .windows(expected_stdout.len())
                .any(|window| window == expected_stdout)
        {
            return Err(format!(
                "stdout missing marker from {}; stdout={:?}",
                self.case.stdout_contains.display(),
                logs.stdout
            )
            .into());
        }
        if !logs.stderr.is_empty() {
            return Err(format!("unexpected stderr: {:?}", logs.stderr).into());
        }
        Ok((elapsed, parse_phases(&logs.stdout)?))
    }

`src/apps/testing/src/bench/execution.rs:191:22`

````rust
parse_phases
````

### usage in `retained_phase_protocol_is_accepted`
#[test]
    fn retained_phase_protocol_is_accepted() {
        let phases = parse_phases(b"noise\nPHASE compute us=42 ok=7\n").unwrap();
        assert_eq!(phases, vec![("compute".to_owned(), 42, 7)]);
        assert!(parse_phases(b"PHASE compute ms=42 ok=7\n").is_err());
    }

`src/apps/testing/src/bench/execution.rs:287:22`

````rust
parse_phases
````
