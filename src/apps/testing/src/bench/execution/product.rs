use super::*;
use std::{error::Error as StdError, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProductBackend {
    ExplicitC,
    DefaultC,
}

impl ProductBackend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::ExplicitC => "explicit-c",
            Self::DefaultC => "default-c",
        }
    }

    fn execution(self) -> hl_container::Execution {
        match self {
            Self::ExplicitC => hl_container::Execution::retained_c(),
            Self::DefaultC => hl_container::Execution::native(false),
        }
    }
}

pub struct ProductSample {
    pub round: u32,
    pub position: u8,
    pub backend: ProductBackend,
    pub setup_us: u128,
    pub execution_us: u128,
    pub teardown_us: u128,
    pub total_us: u128,
    pub output_identity: String,
}

pub struct ProductRun {
    pub setup: BTreeMap<String, u128>,
    pub samples: Vec<ProductSample>,
}

#[derive(Debug)]
struct ProductArmError {
    round: u32,
    position: usize,
    backend: ProductBackend,
    stage: &'static str,
    source: Error,
}

impl fmt::Display for ProductArmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "product A/B round {} position {} backend {} {}: {}",
            self.round,
            self.position,
            self.backend.name(),
            self.stage,
            self.source
        )
    }
}

impl StdError for ProductArmError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

fn product_arm_error(
    round: u32,
    position: usize,
    backend: ProductBackend,
    stage: &'static str,
    source: impl Into<Error>,
) -> Error {
    Box::new(ProductArmError {
        round,
        position,
        backend,
        stage,
        source: source.into(),
    })
}

pub async fn run_product_ab(
    benchmark: Arc<Benchmark>,
    case_index: usize,
    target: Target,
    prepared: Preparation,
    rounds: u32,
) -> std::result::Result<ProductRun, Error> {
    let case = &benchmark.cases[case_index];
    let invocations = rounds.checked_mul(2).ok_or("product A/B invocation count overflow")?;
    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(case.timeout)
            .checked_mul(invocations)
            .and_then(|duration| duration.checked_add(SETUP_ALLOWANCE))
            .ok_or("product A/B deadline overflow")?;
    let image_started = Instant::now();
    let image = tokio::time::timeout_at(deadline, TestImage::materialize(&benchmark.image, &target.platform()))
        .await
        .map_err(|_| "product A/B image materialization exceeded its deadline")??;
    if image.identity() != prepared.image_identity {
        image.release()?;
        return Err("benchmark image identity changed after product A/B preparation".into());
    }
    if let (Some(artifact), Some(identity)) = (&prepared.artifact, &prepared.artifact_identity)
        && crate::record::FramedIdentity::of_file(artifact)? != *identity
    {
        image.release()?;
        return Err("benchmark artifact identity changed after product A/B preparation".into());
    }
    let mut setup = prepared.setup;
    setup.insert("image_materialize".into(), elapsed_us(image_started));
    let outcome = run_product_ab_with_image(
        &benchmark,
        case,
        target,
        image.path(),
        prepared.artifact.as_deref(),
        rounds,
        deadline,
        &mut setup,
    )
    .await;
    let release_started = Instant::now();
    let release = tokio::time::timeout(
        CLEANUP_TIMEOUT,
        tokio::task::spawn_blocking(move || image.release().map_err(|error| error.to_string())),
    )
    .await
    .map_err(|_| "product A/B image cleanup timed out")??;
    release?;
    setup.insert("image_release".into(), elapsed_us(release_started));
    Ok(ProductRun {
        setup,
        samples: outcome?,
    })
}

async fn run_product_ab_with_image(
    benchmark: &Benchmark,
    case: &BenchmarkCase,
    target: Target,
    image: &std::path::Path,
    artifact: Option<&std::path::Path>,
    rounds: u32,
    deadline: tokio::time::Instant,
    setup: &mut BTreeMap<String, u128>,
) -> std::result::Result<Vec<ProductSample>, Error> {
    let started = Instant::now();
    let state = tempfile::tempdir()?;
    let containers = tokio::time::timeout_at(
        deadline,
        hl_container::Containers::builder(Config::new(state.path())).build(),
    )
    .await
    .map_err(|_| "product A/B container service setup exceeded its deadline")??;
    setup.insert("container_service".into(), elapsed_us(started));
    let program = benchmark
        .rootfs_executable
        .clone()
        .unwrap_or_else(|| format!("/opt/husklet/bench-{}", case.id));
    let started = Instant::now();
    if let Some(artifact) = artifact {
        stage(artifact, image, &program)?;
    }
    setup.insert("guest_stage".into(), elapsed_us(started));
    let expected_stdout = tokio::fs::read(&case.stdout_contains).await?;
    let mut expected_identity = None;
    let mut samples = Vec::with_capacity((rounds * 2) as usize);
    for round in 0..rounds {
        for (position, backend) in product_order(round).into_iter().enumerate() {
            let name_repetition = round * 2 + u32::try_from(position)?;
            let invocation = Invocation::new_with_execution(
                &containers,
                benchmark,
                case,
                target,
                image,
                &program,
                name_repetition,
                backend.execution(),
            )
            .map_err(|error| product_arm_error(round, position, backend, "specification", error))?;
            let outcome = tokio::time::timeout_at(deadline, invocation.execute_captured(&expected_stdout))
                .await
                .map_err(|_| {
                    product_arm_error(
                        round,
                        position,
                        backend,
                        "execution",
                        "exceeded the product A/B deadline",
                    )
                })?
                .map_err(|error| product_arm_error(round, position, backend, "execution", error))?;
            let teardown_started = Instant::now();
            tokio::time::timeout(CLEANUP_TIMEOUT, containers.remove_force(&invocation.name))
                .await
                .map_err(|_| product_arm_error(round, position, backend, "cleanup", "container cleanup timed out"))?
                .map_err(|error| product_arm_error(round, position, backend, "cleanup", error))?;
            let remove_us = elapsed_us(teardown_started);
            let identity =
                crate::record::FramedIdentity::over(&[outcome.stdout.as_slice(), outcome.stderr.as_slice()])?;
            preserve_product_identity(&mut expected_identity, &identity, round, position)?;
            let setup_us = lifecycle_sum(&outcome.lifecycle, &["create", "attach", "start"]);
            let execution_us = lifecycle_sum(&outcome.lifecycle, &["wait_and_drain"]);
            let teardown_us = lifecycle_sum(&outcome.lifecycle, &["output_read"]).saturating_add(remove_us);
            samples.push(ProductSample {
                round,
                position: u8::try_from(position)?,
                backend,
                setup_us,
                execution_us,
                teardown_us,
                total_us: setup_us.saturating_add(execution_us).saturating_add(teardown_us),
                output_identity: identity,
            });
        }
    }
    Ok(samples)
}

fn preserve_product_identity(
    expected: &mut Option<String>,
    observed: &str,
    round: u32,
    position: usize,
) -> Result<(), Error> {
    match expected {
        Some(expected) if expected != observed => {
            Err(format!("product A/B output changed at round {round} position {position}").into())
        }
        Some(_) => Ok(()),
        None => {
            *expected = Some(observed.to_owned());
            Ok(())
        }
    }
}

fn lifecycle_sum(values: &[(String, u128)], names: &[&str]) -> u128 {
    values
        .iter()
        .filter(|(name, _)| names.contains(&name.as_str()))
        .map(|(_, value)| *value)
        .sum()
}

fn product_order(round: u32) -> [ProductBackend; 2] {
    if round.is_multiple_of(2) {
        [ProductBackend::ExplicitC, ProductBackend::DefaultC]
    } else {
        [ProductBackend::DefaultC, ProductBackend::ExplicitC]
    }
}
