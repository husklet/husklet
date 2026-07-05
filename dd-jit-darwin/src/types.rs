/// A published port: the host port forwards to the container port (`docker -p HOST:CONTAINER`).
#[derive(Clone, Debug)]
pub struct PortMap {
    pub host: u16,
    pub container: u16,
}

/// A bind mount: a host directory mounted at a path inside the container (`docker -v HOST:CONTAINER`).
/// `ro` marks the mount read-only (`-v HOST:CONTAINER:ro`): the JIT then fails write-intent syscalls
/// under `container` with EROFS. Default `false` = read-write (the normal `-v src:dst`).
#[derive(Clone, Debug)]
pub struct Volume {
    pub container: String,
    pub host: String,
    pub ro: bool,
}
