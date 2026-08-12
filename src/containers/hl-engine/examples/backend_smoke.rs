use hl_engine::activation::GuestIsa;
use hl_engine::engine::{EngineError, ExitKind};
use hl_engine::launch_plan::RuntimePlan;
use hl_engine::options::Options;
use hl_engine::runtime::Engine;

fn run() -> Result<i32, EngineError> {
    let mut arguments = std::env::args_os().skip(1);
    let guest = arguments.next().ok_or(EngineError::LaunchFailed)?;
    let arguments = std::iter::once(guest.as_encoded_bytes().to_vec())
        .chain(arguments.map(|value| value.as_encoded_bytes().to_vec()))
        .collect();
    let mut options = Options::default();
    options
        .set("HL_EXECUTION_BACKEND", "c", true)
        .map_err(|_| EngineError::LaunchFailed)?;
    let engine = Engine::from_plan(
        GuestIsa::Aarch64,
        RuntimePlan {
            rootfs: None,
            executable_host: None,
            arguments,
            environment: Vec::new(),
            result_path: None,
            options,
        },
    )?;
    let runs = std::env::var("HL_C_BACKEND_SMOKE_RUNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value != 0)
        .unwrap_or(1);
    let mut result = None;
    for _ in 0..runs {
        engine.start()?;
        result = Some(engine.wait()?);
    }
    let result = result.expect("at least one C backend smoke run");
    hl_log::hl_event!(
        hl_log::tag::EXEC,
        hl_log::Level::Debug,
        "c_backend_smoke.completed",
        kind = ?result.kind,
        status = result.guest_status,
        detail = result.detail
    );
    Ok(if result.kind == ExitKind::Code {
        result.guest_status
    } else {
        128_i32.saturating_add(result.guest_status)
    })
}

fn main() {
    hl_log::EnvironmentConfig::parse(hl_log::Config::default(), std::env::vars()).apply();
    match run() {
        Ok(status) => std::process::exit(status),
        Err(error) => {
            hl_log::hl_error!(hl_log::tag::EXEC, "c backend smoke failed: error={error:?}");
            std::process::exit(125);
        }
    }
}
