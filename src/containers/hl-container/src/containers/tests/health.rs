use super::*;

#[tokio::test(start_paused = true)]
async fn health_monitor_persists_success_and_inherits_process_context() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(80);
    *runtime.health.lock().unwrap() = Some((
        Duration::from_millis(1),
        std::collections::VecDeque::from([ExitStatus::Code(0)]),
    ));
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let check = Healthcheck::new(Check::Command(Process::new("/health")))
        .interval(Duration::from_millis(5))
        .timeout(Duration::from_millis(20));
    containers.create(spec("healthy").healthcheck(check)).await.unwrap();
    containers.start("healthy").await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let running = containers.inspect("healthy").await.unwrap();
    let health = running.health.expect("durable health state");
    assert_eq!(health.status, HealthStatus::Healthy);
    assert!(!health.probes.is_empty());
    assert_eq!(health.probes.back().unwrap().output, "fake-out\nfake-err\n");
    {
        let programs = runtime.programs.lock().unwrap();
        let parent = &programs[0];
        let probe = programs.iter().find(|process| process.program == "/health").unwrap();
        assert_eq!(probe.env, parent.env);
        assert_eq!(probe.working_dir, parent.working_dir);
        assert_eq!(probe.uid, parent.uid);
        assert_eq!(probe.gid, parent.gid);
    }
    {
        let values = runtime.mounts.lock().unwrap();
        assert_eq!(values[0], values[1]);
    }
    {
        let values = runtime.networks.lock().unwrap();
        assert_eq!(values[0], values[1]);
    }
    {
        let values = runtime.resources.lock().unwrap();
        assert_eq!(values[0], values[1]);
    }
    {
        let values = runtime.isolations.lock().unwrap();
        assert_eq!(values[0], values[1]);
    }
    {
        let domains = runtime.domains.lock().unwrap();
        assert!(domains.len() >= 2);
        let parent = domains[0].0;
        assert!(domains[0].1);
        assert!(domains[1..].iter().all(|domain| domain.0 == parent && !domain.1));
    }
    assert!(runtime.domain_reads.load(Ordering::SeqCst) >= 1);
    containers.wait("healthy").await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn start_interval_drives_grace_probes_and_stop_cancels_an_inflight_probe() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(50);
    *runtime.health.lock().unwrap() = Some((
        Duration::from_secs(1),
        std::collections::VecDeque::from([ExitStatus::Code(1)]),
    ));
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let check = Healthcheck::new(Check::Command(Process::new("/health")))
        .interval(Duration::from_secs(1))
        .start_period(Duration::from_secs(1))
        .start_interval(Duration::from_millis(2))
        .timeout(Duration::from_secs(2));
    containers
        .create(spec("cancel-health").healthcheck(check))
        .await
        .unwrap();
    containers.start("cancel-health").await.unwrap();

    tokio::time::timeout(Duration::from_millis(100), async {
        while runtime.programs.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    containers.stop("cancel-health", Duration::ZERO).await.unwrap();
    let probes = runtime
        .programs
        .lock()
        .unwrap()
        .iter()
        .filter(|process| process.program == "/health")
        .count();
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert_eq!(
        runtime
            .programs
            .lock()
            .unwrap()
            .iter()
            .filter(|process| process.program == "/health")
            .count(),
        probes
    );
    assert!(
        runtime
            .signals
            .lock()
            .unwrap()
            .iter()
            .filter(|signal| **signal == Signal::KILL)
            .count()
            >= 2
    );
}

#[tokio::test(start_paused = true)]
async fn shell_health_check_uses_container_shell_context() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(40);
    *runtime.health.lock().unwrap() = Some((
        Duration::from_millis(1),
        std::collections::VecDeque::from([ExitStatus::Code(0)]),
    ));
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let check = Healthcheck::new(Check::Shell("__health__".into()))
        .interval(Duration::from_millis(2))
        .timeout(Duration::from_millis(10));
    containers
        .create(spec("shell-health").healthcheck(check))
        .await
        .unwrap();
    containers.start("shell-health").await.unwrap();
    tokio::time::sleep(Duration::from_millis(12)).await;
    {
        let programs = runtime.programs.lock().unwrap();
        let probe = programs
            .iter()
            .find(|process| process.args.iter().any(|value| value == "__health__"))
            .unwrap();
        assert_eq!(probe.program, "/bin/sh");
        assert_eq!(probe.args, ["-c", "__health__"]);
    }
    containers.wait("shell-health").await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn health_grace_threshold_timeout_and_pause_are_generation_safe() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(0));
    runtime.delay = Duration::from_millis(140);
    *runtime.health.lock().unwrap() = Some((
        Duration::from_millis(20),
        std::collections::VecDeque::from([
            ExitStatus::Code(1),
            ExitStatus::Code(1),
            ExitStatus::Code(1),
            ExitStatus::Code(1),
        ]),
    ));
    let runtime = Arc::new(runtime);
    let containers = service(Arc::clone(&runtime)).await;
    let check = Healthcheck::new(Check::Command(Process::new("/health")))
        .interval(Duration::from_millis(2))
        .timeout(Duration::from_millis(5))
        .start_period(Duration::from_millis(20))
        .start_interval(Duration::from_millis(5))
        .retries(2);
    containers.create(spec("unhealthy").healthcheck(check)).await.unwrap();
    containers.start("unhealthy").await.unwrap();
    let before_pause = tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            let container = containers.inspect("unhealthy").await.unwrap();
            if container
                .health
                .as_ref()
                .is_some_and(|health| health.status == HealthStatus::Unhealthy)
            {
                break container;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("health monitor did not reach the retry threshold");
    let health = before_pause.health.as_ref().unwrap();
    assert_eq!(health.status, HealthStatus::Unhealthy);
    assert!(
        health
            .probes
            .iter()
            .all(|probe| matches!(probe.result, ExitStatus::Fault { status: -1, .. }))
    );
    assert!(runtime.signals.lock().unwrap().contains(&Signal::KILL));
    containers.pause("unhealthy").await.unwrap();
    let count = containers
        .inspect("unhealthy")
        .await
        .unwrap()
        .health
        .unwrap()
        .probes
        .len();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        containers
            .inspect("unhealthy")
            .await
            .unwrap()
            .health
            .unwrap()
            .probes
            .len(),
        count
    );
    containers.unpause("unhealthy").await.unwrap();
    containers.wait("unhealthy").await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn late_probe_from_previous_generation_cannot_mutate_restart() {
    let mut runtime = FakeRuntime::new(ExitStatus::Code(1));
    runtime.delay = Duration::from_millis(15);
    *runtime.health.lock().unwrap() = Some((
        Duration::from_millis(80),
        std::collections::VecDeque::from([ExitStatus::Code(0)]),
    ));
    let runtime = Arc::new(runtime);
    let containers = service(runtime).await;
    let check = Healthcheck::new(Check::Command(Process::new("/health")))
        .interval(Duration::from_millis(1))
        .timeout(Duration::from_millis(100));
    containers
        .create(
            spec("generation-health")
                .healthcheck(check)
                .restart(RestartPolicy::OnFailure { maximum: Some(1) }),
        )
        .await
        .unwrap();
    containers.start("generation-health").await.unwrap();
    assert_eq!(containers.wait("generation-health").await.unwrap(), ExitStatus::Code(1));
    tokio::time::sleep(Duration::from_millis(90)).await;
    let container = containers.inspect("generation-health").await.unwrap();
    assert_eq!(container.generation, 2);
    assert!(container.health.unwrap().probes.is_empty());
}
