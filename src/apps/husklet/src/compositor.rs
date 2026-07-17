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
        remove_socket(&socket)?;

        if presentation == Presentation::Native {
            return start_native(socket);
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
        while !socket.exists() {
            if let Ok(result) = finished_rx.try_recv() {
                let _ = thread.join();
                return result.and_then(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "compositor stopped during startup",
                    ))
                });
            }
            if Instant::now() >= deadline {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "compositor socket was not ready",
                ));
            }
            thread::sleep(Duration::from_millis(5));
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
}

fn remove_socket(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        match self.worker.take() {
            Some(Worker::Thread(thread)) => {
                let _ = thread.join();
            }
            #[cfg(target_os = "macos")]
            Some(Worker::Process { mut child, control }) => {
                drop(control);
                let _ = child.wait();
            }
            None => {}
        }
        let _ = remove_socket(&self.socket);
    }
}

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
        let control = child.stdin.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "compositor control pipe was not created",
            )
        })?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !socket.exists() {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("native compositor stopped during startup with {status}"),
                ));
            }
            if Instant::now() >= deadline {
                drop(control);
                let _ = child.wait();
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
pub fn run_native(socket: &Path) -> io::Result<()> {
    use std::io::Read;

    use hl_compositor::surface::macos::MacPresenter;

    // Build the Wayland output from the actual host display before HlState advertises its globals. GTK,
    // Chrome, and other scale-aware clients then allocate Retina-sized buffers instead of having a
    // scale-1 buffer stretched by Core Animation. An explicit layout remains useful for tests/multi-output.
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
    let mut presenter = MacPresenter::new_windowed_on_main_thread().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "native compositor requires the process main thread and a Metal presentation device",
        )
    })?;
    if let Some(directory) = std::env::var_os("HL_NATIVE_CAPTURE_DIR") {
        presenter = presenter.capture_to(directory)?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        let mut buffer = [0_u8; 1];
        while input.read(&mut buffer).ok().is_some_and(|read| read != 0) {}
        reader_stop.store(true, Ordering::Release);
    });
    smithay::run(socket, presenter, stop)
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
        drop(service);
        assert!(!socket.exists());
        let _ = std::fs::remove_dir_all(root);
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
