//! Wayland compositor lifecycle owned by the application composition root.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{self, PngPresenter};

pub struct Service {
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<Worker>,
}

enum Worker {
    Thread(JoinHandle<()>),
    #[cfg(target_os = "macos")]
    Process {
        child: Child,
        control: ChildStdin,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Headless,
    Native,
}

impl Presentation {
    pub fn configured() -> io::Result<Presentation> {
        match std::env::var("HL_PRESENTATION").as_deref() {
            Ok("headless") => Ok(Presentation::Headless),
            Ok("native") => Ok(Presentation::Native),
            Err(std::env::VarError::NotPresent) => {
                #[cfg(target_os = "macos")]
                return Ok(Presentation::Native);
                #[cfg(not(target_os = "macos"))]
                return Ok(Presentation::Headless);
            }
            Err(std::env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HL_PRESENTATION is not valid UTF-8",
            )),
            Ok(value) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported HL_PRESENTATION {value:?}; expected native or headless"),
            )),
        }
    }
}

impl Service {
    pub fn start(socket: impl Into<PathBuf>, frames: impl Into<PathBuf>) -> io::Result<Self> {
        Self::start_with(socket, frames, Presentation::Headless)
    }

    pub fn start_with(
        socket: impl Into<PathBuf>,
        frames: impl Into<PathBuf>,
        presentation: Presentation,
    ) -> io::Result<Self> {
        let socket = socket.into();
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::remove_socket(&socket)?;

        if presentation == Presentation::Native {
            return Self::start_native(socket);
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_socket = socket.clone();
        let frames = frames.into();
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let result = smithay::run(
                &thread_socket,
                PngPresenter::with_png_dir(frames),
                thread_stop,
            );
            let _ = finished_tx.send(result);
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if Self::is_up(&socket) {
                break;
            }
            if Instant::now() >= deadline {
                stop.store(true, Ordering::Release);
                if thread.join().is_err() {
                    hl_log::hl_error!(
                        hl_log::tag::COMPOSITOR,
                        "headless compositor panicked during startup timeout"
                    );
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "compositor socket was not ready",
                ));
            }
            let Ok(result) = finished_rx.try_recv() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let error = match result {
                Ok(()) => io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "compositor stopped during startup",
                ),
                Err(error) => error,
            };
            if thread.join().is_err() {
                hl_log::hl_error!(
                    hl_log::tag::COMPOSITOR,
                    "headless compositor panicked during failed startup"
                );
            }
            return Err(error);
        }

        Ok(Self {
            socket,
            stop,
            worker: Some(Worker::Thread(thread)),
        })
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn remove_socket(path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn is_up(path: &Path) -> bool {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }

    #[cfg(target_os = "macos")]
    fn stop_native(mut child: Child, control: ChildStdin, timeout: Duration) -> io::Result<()> {
        drop(control);
        let deadline = Instant::now() + timeout;
        loop {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        match self.worker.take() {
            Some(Worker::Thread(thread)) => {
                if thread.join().is_err() {
                    hl_log::hl_error!(
                        hl_log::tag::COMPOSITOR,
                        "headless compositor thread panicked during shutdown"
                    );
                }
            }
            #[cfg(target_os = "macos")]
            Some(Worker::Process { child, control }) => {
                if let Err(error) = Self::stop_native(child, control, Duration::from_secs(2)) {
                    hl_log::hl_error!(
                        hl_log::tag::COMPOSITOR,
                        "native compositor shutdown failed error={error}"
                    );
                }
            }
            None => {}
        }
        if let Err(error) = Service::remove_socket(&self.socket) {
            hl_log::hl_error!(
                hl_log::tag::COMPOSITOR,
                "compositor socket cleanup failed path={} error={error}",
                self.socket.display()
            );
        }
    }
}

impl Service {
    fn start_native(socket: PathBuf) -> io::Result<Service> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = socket;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native presentation is currently supported only on macOS",
            ));
        }

        #[cfg(target_os = "macos")]
        {
            let executable = std::env::current_exe()?;
            let mut child = Command::new(executable)
                .arg("__compositor")
                .arg("--socket")
                .arg(&socket)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()?;
            let control = match child.stdin.take() {
                Some(control) => control,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "compositor control pipe was not created",
                    ));
                }
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            while !Self::is_up(&socket) {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            format!("native compositor stopped during startup with {status}"),
                        ));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = Self::stop_native(child, control, Duration::from_secs(2));
                        return Err(error);
                    }
                }
                if Instant::now() >= deadline {
                    Self::stop_native(child, control, Duration::from_secs(2))?;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "native compositor socket was not ready",
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(Service {
                socket,
                stop: Arc::new(AtomicBool::new(false)),
                worker: Some(Worker::Process { child, control }),
            })
        }
    }

    #[cfg(target_os = "macos")]
    pub fn run_native(socket: &Path, configuration: NativeConfiguration) -> io::Result<()> {
        use hl_compositor::surface::macos::MacPresenter;

        let mut presenter = MacPresenter::new_windowed_on_main_thread().ok_or(io::Error::new(
            io::ErrorKind::NotFound,
            "native compositor requires the process main thread and a Metal presentation device",
        ))?;
        if let Some(directory) = configuration.capture_directory {
            presenter = presenter.capture_to(directory)?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        NativeControl::watch(Arc::clone(&stop));
        smithay::run(socket, presenter, stop)
    }
}

#[cfg(target_os = "macos")]
struct NativeControl;

#[cfg(target_os = "macos")]
impl NativeControl {
    fn watch(stop: Arc<AtomicBool>) {
        thread::spawn(move || Self::read_until_closed(stop));
    }

    fn read_until_closed(stop: Arc<AtomicBool>) {
        use std::io::Read;

        let mut input = std::io::stdin().lock();
        let mut buffer = [0_u8; 1];
        while input.read(&mut buffer).ok().is_some_and(|read| read != 0) {}
        stop.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
pub struct NativeConfiguration {
    capture_directory: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
impl NativeConfiguration {
    pub fn configured() -> Self {
        use hl_compositor::surface::macos::MacPresenter;

        // Smithay reads these compatibility values while constructing its globals, so resolve the host
        // display at the executable composition boundary before the service starts.
        if std::env::var_os("HL_OUTPUTS").is_none() {
            if let Some(spec) = MacPresenter::primary_output_spec_on_main_thread() {
                std::env::set_var("HL_OUTPUTS", spec);
            }
        }
        if std::env::var_os("HL_OUTPUT_REFRESH_MHZ").is_none() {
            if let Some(refresh) = MacPresenter::primary_refresh_millihz_on_main_thread() {
                std::env::set_var("HL_OUTPUT_REFRESH_MHZ", refresh.to_string());
            }
        }
        Self {
            capture_directory: std::env::var_os("HL_NATIVE_CAPTURE_DIR").map(PathBuf::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishes_and_removes_wayland_socket() {
        let root =
            std::env::temp_dir().join(format!("hl-compositor-service-{}", std::process::id()));
        let socket = root.join("wayland-0");
        let service = Service::start(&socket, root.join("frames")).unwrap();
        assert_eq!(service.socket(), socket);
        assert!(socket.exists());
        assert!(Service::is_up(&socket));
        drop(service);
        assert!(!socket.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_worker_shutdown_is_bounded_when_control_close_is_ignored() {
        let mut child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let control = child.stdin.take().unwrap();
        let started = Instant::now();

        Service::stop_native(child, control, Duration::from_millis(30)).unwrap();

        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn rejects_native_presentation_on_an_unsupported_host() {
        let root = std::env::temp_dir().join(format!("hl-native-service-{}", std::process::id()));
        let error = Service::start_with(
            root.join("wayland-0"),
            root.join("frames"),
            Presentation::Native,
        )
        .err()
        .expect("native presentation must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
