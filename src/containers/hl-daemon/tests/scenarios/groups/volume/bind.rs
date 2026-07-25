use std::{env, path::Path};

use crate::report::ScenarioBatch;
use hl_client::Client;
use tempfile::TempDir;

use super::{
    execution::{execute, pass},
    Error,
};

#[allow(
    clippy::too_many_lines,
    reason = "declarative bind-volume compatibility table"
)]
pub(super) async fn binds(
    client: &Client,
    scenarios: &std::collections::BTreeMap<&str, crate::contract::Scenario>,
    reports: &mut ScenarioBatch,
) -> Result<(), Error> {
    const CASES: [(&str, &str, &str, &str); 16] = [
        (
            "volumes/write-seen-on-host",
            "",
            "echo CWROTE > /data/f.txt",
            "",
        ),
        (
            "volumes/host-seen-in-container",
            "h.txt:HSEED\n",
            "cat /data/h.txt",
            "HSEED\n",
        ),
        (
            "volumes/readonly-rejects-write",
            "",
            "echo x > /data/n 2>/dev/null || echo RO_REJECTED",
            "RO_REJECTED\n",
        ),
        (
            "volumes/delete-propagates",
            "d.txt:a\n",
            "rm /data/d.txt; echo DELETED",
            "DELETED\n",
        ),
        (
            "volumes/persist-across-runs",
            "",
            "echo persisted > /data/p; cat /data/p",
            "persisted\n",
        ),
        (
            "volumes/subdir-mount",
            "inner.txt:inner\n",
            "cat /data/inner.txt; ls /data",
            "inner\ninner.txt\n",
        ),
        (
            "volumes/two-mounts",
            "a:one\n",
            "cat /data/a; cat /other/b",
            "one\ntwo\n",
        ),
        (
            "volumes/nested-dotdot-crosses-boundary",
            "HOSTMARK:host-sibling\n",
            "ls /data/.. | grep -qx etc && ls /data/.. | grep -qx bin && echo PARENT_IS_ROOTFS; ls /data/.. | grep -q HOSTMARK && echo LEAKED || echo NO_LEAK",
            "PARENT_IS_ROOTFS\nNO_LEAK\n",
        ),
        (
            "volumes/cmd-cat-grep-wc",
            "f:apple\nbanana\ncherry\n",
            "cat /data/f | grep a | wc -l | tr -d ' '",
            "2\n",
        ),
        (
            "volumes/cmd-cp-mv-rm",
            "a:hi\n",
            "cp /data/a /data/b && mv /data/b /data/c && rm /data/a && ls /data | sort | tr '\n' ','",
            "c,",
        ),
        (
            "volumes/cmd-sed-inplace",
            "cfg:foo=1\n",
            "sed -i s/foo/bar/ /data/cfg; cat /data/cfg",
            "bar=1\n",
        ),
        (
            "volumes/cmd-append-redirect",
            "log:one\n",
            "echo two >> /data/log; tr '\n' ' ' < /data/log",
            "one two ",
        ),
        (
            "volumes/cmd-sort-head-tail",
            "n:3\n1\n2\n",
            "echo MIN=$(sort /data/n | head -1); echo MAX=$(sort /data/n | tail -1)",
            "MIN=1\nMAX=3\n",
        ),
        (
            "volumes/cmd-chmod-perms",
            "s:x\n",
            "chmod 640 /data/s && ls -l /data/s | cut -c1-10",
            "-rw-r-----\n",
        ),
        (
            "volumes/cmd-mkdir-touch-find",
            "",
            "mkdir -p /data/x/y && touch /data/x/y/z.txt && find /data -name z.txt",
            "/data/x/y/z.txt\n",
        ),
        (
            "volumes/cmd-wc-bytes",
            "b:abcde",
            "wc -c < /data/b | tr -d ' '",
            "5\n",
        ),
    ];
    let selected = env::var("HL_VOLUME_CASE").ok();
    let mut failures = Vec::new();
    for (index, (id, seed, command, expected)) in CASES.iter().enumerate() {
        if selected.as_deref().is_some_and(|selected| selected != *id) {
            continue;
        }
        let scenario = &scenarios[id];
        let Some(attempt) = reports.begin(scenario)? else {
            println!("RESUME {id}");
            continue;
        };
        let host = TempDir::new()?;
        seed_files(host.path(), seed)?;
        let other = TempDir::new()?;
        let mut mounts = vec![format!("{}:/data", host.path().display())];
        if *id == "volumes/readonly-rejects-write" {
            mounts[0].push_str(":ro");
        }
        if *id == "volumes/two-mounts" {
            std::fs::write(other.path().join("b"), b"two\n")?;
            mounts.push(format!("{}:/other", other.path().display()));
        }
        let execution = execute(client, &format!("bind-{index}"), command, mounts).await;
        let result: Result<(), Error> = match execution {
            Ok(output) => {
                let state = match *id {
                    "volumes/write-seen-on-host" => std::fs::read(host.path().join("f.txt"))
                        .is_ok_and(|bytes| bytes == b"CWROTE\n"),
                    "volumes/delete-propagates" => !host.path().join("d.txt").exists(),
                    _ => true,
                };
                if let Err(error) = pass(output == expected.as_bytes() && state, id) {
                    eprintln!(
                        "FAIL {id}: {error}; stdout={:?} expected={:?}",
                        String::from_utf8_lossy(&output),
                        expected
                    );
                    failures.push(*id);
                    Err(error)
                } else {
                    Ok(())
                }
            }
            Err(error) => {
                eprintln!("FAIL {id}: {error}");
                failures.push(*id);
                Err(error)
            }
        };
        reports.complete(scenario, attempt, &result)?;
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} volume cases failed: {}",
            failures.len(),
            failures.join(", ")
        )
        .into())
    }
}
fn seed_files(root: &Path, seed: &str) -> Result<(), Error> {
    if let Some((name, bytes)) = seed.split_once(':') {
        std::fs::write(root.join(name), bytes)?;
    }
    Ok(())
}
