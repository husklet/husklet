/// A published port: the host port forwards to the container port (`docker -p HOST:CONTAINER`).
#[derive(Clone, Debug)]
pub struct PortMap {
    /// Host-side port that accepts connections (the `HOST` in `-p HOST:CONTAINER`).
    pub host: u16,
    /// Container-side port those connections are forwarded to (the `CONTAINER`).
    pub container: u16,
}

/// A bind mount: a host directory mounted at a path inside the container (`docker -v HOST:CONTAINER`).
/// `ro` marks the mount read-only (`-v HOST:CONTAINER:ro`): the JIT then fails write-intent syscalls
/// under `container` with EROFS. Default `false` = read-write (the normal `-v src:dst`).
#[derive(Clone, Debug)]
pub struct Volume {
    /// Path inside the container where the host directory is mounted (the `CONTAINER` in `-v HOST:CONTAINER`).
    pub container: String,
    /// Host directory that is bind-mounted (the `HOST` in `-v HOST:CONTAINER`).
    pub host: String,
    /// Mount read-only (`-v HOST:CONTAINER:ro`): write-intent syscalls under `container` fail EROFS.
    pub ro: bool,
}
