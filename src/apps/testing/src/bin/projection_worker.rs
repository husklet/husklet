fn main() {
    Worker::run();
}

struct Worker;

impl Worker {
    fn run() {
        let arguments = std::env::args().collect::<Vec<_>>();
        let descriptor = arguments
            .windows(2)
            .find(|pair| pair[0] == "--authority-fd")
            .and_then(|pair| pair[1].parse::<i32>().ok());
        let health = arguments
            .windows(2)
            .find(|pair| pair[0] == "--health-fd")
            .and_then(|pair| pair[1].parse::<i32>().ok());
        let path = arguments
            .windows(2)
            .find(|pair| pair[0] == "--replace")
            .map(|pair| std::path::PathBuf::from(&pair[1]));
        let result = descriptor
            .zip(health)
            .zip(path)
            .ok_or(1)
            .and_then(|((descriptor, health), path)| Self::project(descriptor, health, &path));
        if let Err(stage) = result {
            std::process::exit(70 + i32::from(stage));
        }
    }

    fn project(descriptor: i32, health: i32, path: &std::path::Path) -> Result<(), u8> {
        let mut authority = hl_engine::native::AuthorityWorker::inherit(descriptor, health).map_err(|_| 2)?;
        authority.enter(|| ()).map_err(|_| 3)?;
        std::fs::remove_file(path).map_err(|_| 4)?;
        std::fs::write(path, b"replacement").map_err(|_| 5)?;
        hl_engine::native::HostConfinement::apply().map_err(|_| 6)?;
        let direct = std::fs::File::open("/etc/passwd").is_err_and(|error| error.raw_os_error() == Some(libc::EPERM));
        if !direct {
            return Err(7);
        }
        let handle = authority.open_file(1).map_err(|_| 8)?;
        let info = authority.file_info(handle).map_err(|_| 9)?;
        if info.size != 20 || info.mode & libc::S_IFMT != libc::S_IFREG || info.device == 0 || info.inode == 0 {
            return Err(10);
        }
        let bytes = authority.read_file(handle, 0, 64).map_err(|_| 9)?;
        if bytes != b"authority-projection" {
            return Err(10);
        }
        authority.close_file(handle).map_err(|_| 11)?;
        if authority.read_file(handle, 0, 1) != Err(hl_engine::native::ProjectionError::Linux(libc::EBADF)) {
            return Err(12);
        }
        authority.close().map_err(|_| 13)
    }
}
