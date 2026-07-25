//! Real upstream software workflow: Redis, Python, `PostgreSQL`, and NATS.

use hl_container::{ContainerSpec, Containers, ExitStatus, Isolation, Process, Sandbox};
use hl_images::{
    remote::{Auth, Registry},
    Images, Reference,
};
use std::time::Duration;

type Error = Box<dyn std::error::Error>;

pub(super) async fn run(containers: &Containers) -> Result<(), Error> {
    let platform = crate::contract::Target::from_env()?.platform();
    let cache = crate::fixture::cache_root(&platform)?;
    let images = Images::open(cache)?;
    finite(
        containers, &images, "redis:alpine", "realsw-redis",
        Process::new("/bin/sh").args(["-c", "P=/usr/local/bin; \"$P/redis-server\" --save '' --appendonly no --daemonize no >/tmp/redis.log 2>&1 & sleep 3; echo ping=$(\"$P/redis-cli\" ping); \"$P/redis-cli\" set k hello-redis >/dev/null; echo get=$(\"$P/redis-cli\" get k); \"$P/redis-cli\" incr ctr >/dev/null; echo incr=$(\"$P/redis-cli\" incr ctr)"]),
        &["ping=PONG", "get=hello-redis", "incr=2"], 90,
    ).await?;
    finite(
        containers, &images, "python:alpine", "realsw-python",
        Process::new("python3").args(["-c", "import functools\n@functools.lru_cache(None)\ndef fib(n): return n if n<2 else fib(n-1)+fib(n-2)\nd={}\nfor i in range(100000): d[i%1000]=d.get(i%1000,0)+i\nprint('py', 'fib35='+str(fib(35)), 'dictsum='+str(sum(d.values())), 'sorted='+str(sorted([3,1,2])))"]),
        &["py fib35=9227465 dictsum=4999950000 sorted=[1, 2, 3]"], 90,
    ).await?;
    finite(
        containers, &images, "postgres:alpine", "realsw-postgres",
        Process::new("/bin/sh").args(["-c", "set -eu; export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin PGDATA=/var/lib/postgresql/data; mkdir -p \"$PGDATA\"; chown postgres:postgres \"$PGDATA\"; gosu postgres initdb -D \"$PGDATA\" --auth=trust >/tmp/initdb.log; gosu postgres pg_ctl -D \"$PGDATA\" -o '-k /tmp' -w start >/tmp/pg.log; echo postgres-ready; psql -h /tmp -U postgres -tAc 'CREATE TABLE t(v int); INSERT INTO t SELECT generate_series(1,1000); SELECT count(*), sum(v) FROM t;'; gosu postgres pg_ctl -D \"$PGDATA\" -m fast -w stop >/dev/null "]),
        &["postgres-ready", "1000|500500"], 90,
    ).await?;
    nats(containers, &images).await?;
    if !containers.list().await?.is_empty() {
        return Err("real-software workflow leaked container records".into());
    }
    Ok(())
}

async fn finite(
    containers: &Containers,
    images: &Images,
    raw: &str,
    name: &str,
    process: Process,
    expected: &[&str],
    seconds: u64,
) -> Result<(), Error> {
    let (root, view) = rootfs(images, raw).await?;
    let outcome = async {
        containers
            .create(
                ContainerSpec::from_directory(view.path(), process)
                    .name(name)
                    .isolation(Isolation {
                        sandbox: Sandbox::Disabled,
                        network_isolated: true,
                        ..Isolation::default()
                    }),
            )
            .await?;
        containers.start(name).await?;
        let status =
            tokio::time::timeout(Duration::from_secs(seconds), containers.wait(name)).await??;
        let logs = containers.logs(name).await?;
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        );
        if status != ExitStatus::Code(0) || expected.iter().any(|marker| !output.contains(marker)) {
            return Err::<(), Error>(
                format!("{name}: status={status:?} expected={expected:?} output={output:?}").into(),
            );
        }
        for marker in expected {
            println!("PASS {name} {marker}");
        }
        Ok(())
    }
    .await;
    let cleanup = cleanup(containers, name, images, &root).await;
    combine(outcome, cleanup)
}

async fn nats(containers: &Containers, images: &Images) -> Result<(), Error> {
    let (root, view) = rootfs(images, "nats:latest").await?;
    let name = "realsw-nats";
    let outcome = async {
        containers
            .create(
                ContainerSpec::from_directory(view.path(), Process::new("/nats-server"))
                    .name(name)
                    .isolation(Isolation {
                        sandbox: Sandbox::Disabled,
                        network_isolated: true,
                        ..Isolation::default()
                    }),
            )
            .await?;
        containers.start(name).await?;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let logs = containers.logs(name).await?;
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&logs.stdout),
            String::from_utf8_lossy(&logs.stderr)
        );
        if !output.contains("Server is ready") {
            return Err::<(), Error>(format!("nats readiness output={output:?}").into());
        }
        println!("PASS realsw-nats Server is ready");
        Ok(())
    }
    .await;
    let cleanup = cleanup(containers, name, images, &root).await;
    combine(outcome, cleanup)
}

async fn cleanup(
    containers: &Containers,
    name: &str,
    images: &Images,
    root: &hl_images::rootfs::Reference,
) -> Result<(), Error> {
    let remove = if containers.inspect(name).await.is_ok() {
        containers
            .remove_force(name)
            .await
            .map(|_| ())
            .map_err(Error::from)
    } else {
        Ok(())
    };
    let release = images.roots().release(root).map_err(Error::from);
    let closed = images.roots().open(root).is_err();
    remove?;
    release?;
    if !closed {
        return Err("real-software rootfs lease remained open after release".into());
    }
    Ok(())
}

fn combine(outcome: Result<(), Error>, cleanup: Result<(), Error>) -> Result<(), Error> {
    match (outcome, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}").into()),
    }
}

async fn rootfs(
    images: &Images,
    raw: &str,
) -> Result<(hl_images::rootfs::Reference, hl_images::rootfs::View), Error> {
    let platform = crate::contract::Target::from_env()?.platform();
    let reference: Reference = raw.parse()?;
    let image = match images.resolve(&reference)? {
        Some(image) => image,
        None => {
            images
                .pull(&Registry::new(Auth::Anonymous), reference, &platform)
                .await?
        }
    };
    let unpacked = images.unpack(&image, &platform)?;
    let root = images.rootfs(&unpacked)?;
    let view = images.roots().open(&root)?;
    Ok((root, view))
}
