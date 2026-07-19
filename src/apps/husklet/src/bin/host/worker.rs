pub(crate) struct Worker;

impl Worker {
    pub(crate) fn run() -> Option<i32> {
        let mut arguments = std::env::args().skip(1);
        if arguments.next().as_deref() != Some("--worker") {
            return None;
        }
        let operation = arguments.next().unwrap_or_default();
        let name = arguments.next().unwrap_or_default();
        let slot = arguments.next().filter(|value| !value.is_empty());
        Some(match operation.as_str() {
            "launch" => {
                let restore = arguments.next().as_deref() == Some("restore");
                let cwd = arguments.next().filter(|value| !value.is_empty());
                hl::runtime::worker::Worker::launch(&name, restore, cwd.as_deref(), slot.as_deref())
            }
            "checkpoint" => hl::runtime::worker::Worker::checkpoint(&name, slot.as_deref()),
            "daemon" => match hl::runtime::worker::Worker::daemon(&name) {
                Ok(socket) => {
                    println!("{}", socket.display());
                    0
                }
                Err(error) => {
                    eprintln!("workspace resources unavailable: {error}");
                    1
                }
            },
            _ => {
                eprintln!("invalid Husklet worker operation");
                2
            }
        })
    }
}
