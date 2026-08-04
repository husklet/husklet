//! Interactive terminal transcript workflow driven through the Rust session API.

use crate::fixture::Fixture;
use hl_container::{Console, ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox, Size};
use std::time::Duration;

type Error = Box<dyn std::error::Error>;

struct Case {
    name: &'static str,
    image: &'static str,
    program: &'static str,
    arguments: &'static [&'static str],
    writes: &'static [&'static [u8]],
    expected: &'static str,
    reject_del: bool,
}

pub(super) async fn run(containers: &Containers) -> Result<(), Error> {
    for case in cases() {
        execute(containers, &case).await?;
    }
    if !containers.list().await?.is_empty() {
        return Err("PTY workflow leaked container records".into());
    }
    Ok(())
}

async fn execute(containers: &Containers, case: &Case) -> Result<(), Error> {
    let fixture = Fixture::materialize(case.image).await?;
    let name = format!("pty-{}", case.name);
    let mut process = Process::new(case.program)
        .args(case.arguments.iter().copied())
        .console(Console {
            stdin: true,
            terminal: Some(Size::new(24, 80)?),
        });
    for (key, value) in &fixture.runtime().environment {
        process = process.env(key, value);
    }
    process = process.env("TERM", "xterm");
    if !fixture.runtime().working_directory.is_empty() {
        process = process.working_dir(&fixture.runtime().working_directory);
    }
    let outcome = async {
        containers
            .create(
                ContainerSpec::from_directory(fixture.path(), process)
                    .name(&name)
                    .isolation(Isolation {
                        sandbox: Sandbox::Disabled,
                        network_isolated: true,
                        ..Isolation::default()
                    }),
            )
            .await?;
        let session = containers.attach(&name).await?;
        containers.start(&name).await?;
        for bytes in case.writes {
            session.write(bytes.to_vec()).await?;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        session.close().await;
        let status = tokio::time::timeout(Duration::from_secs(20), containers.wait(&name)).await??;
        let logs = containers.logs(&name).await?;
        let transcript = [logs.stdout, logs.stderr].concat();
        let text = String::from_utf8_lossy(&transcript);
        if status != ExitStatus::Code(0)
            || !text.contains(case.expected)
            || (case.reject_del && transcript.contains(&0x7f))
        {
            return Err::<(), Error>(
                format!(
                    "{}: status={status:?} expected={:?} transcript={transcript:?}",
                    case.name, case.expected
                )
                .into(),
            );
        }
        println!("PASS pty-conformance/{}", case.name);
        Ok(())
    }
    .await;
    let remove = if containers.inspect(&name).await.is_ok() {
        containers.remove_force(&name).await.map(|_| ()).map_err(Error::from)
    } else {
        Ok(())
    };
    let release = fixture.release();
    match (outcome, remove, release) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (outcome, remove, release) => Err(format!(
            "outcome={:?}; remove={:?}; release={:?}",
            outcome.err().map(|e| e.to_string()),
            remove.err().map(|e| e.to_string()),
            release.err().map(|e| e.to_string())
        )
        .into()),
    }
}

fn cases() -> [Case; 5] {
    const EDIT: &[&[u8]] = &[b"1", b"2", b"8", b"\x7f", b"3", b"\r", b"exit()\r"];
    [
        Case {
            name: "node-repl-backspace",
            image: "node:20-alpine",
            program: "node",
            arguments: &[],
            writes: &[b"1", b"2", b"8", b"\x7f", b"3", b"\r", b".exit\r"],
            expected: "123",
            reject_del: true,
        },
        Case {
            name: "python-repl-backspace",
            image: "python:3.12-alpine",
            program: "python3",
            arguments: &[],
            writes: EDIT,
            expected: "123",
            reject_del: true,
        },
        Case {
            name: "bash-line-backspace",
            image: "ubuntu:latest",
            program: "bash",
            arguments: &[],
            writes: &[b"echo 128", b"\x7f", b"3\r", b"exit\r"],
            expected: "123",
            reject_del: true,
        },
        Case {
            name: "raw-noecho-no-doubleecho",
            image: "python:3.12-alpine",
            program: "python3",
            arguments: &[
                "-c",
                "import termios,os,sys\nt=termios.tcgetattr(0); t[3]&=~(termios.ICANON|termios.ECHO); t[6][termios.VMIN]=1; t[6][termios.VTIME]=0\ntermios.tcsetattr(0,termios.TCSANOW,t); sys.stdout.write('RDY\\r\\n'); sys.stdout.flush()\nb=os.read(0,3); sys.stdout.write('GOT=%r\\r\\n'%b); sys.stdout.flush()",
            ],
            writes: &[b"abc"],
            expected: "GOT=b'abc'",
            reject_del: false,
        },
        Case {
            name: "tty-term-xterm",
            image: "ubuntu:latest",
            program: "bash",
            arguments: &["--norc"],
            writes: &[b"echo T=[$TERM]\r", b"exit\r"],
            expected: "T=[xterm]",
            reject_del: false,
        },
    ]
}
