# Docker create compatibility

## Nullable collection wire contract

Docker CLI 29.1.3 serializes empty create collections inconsistently as omitted,
`null`, empty arrays, or empty objects.  A captured v1.43 `docker create` request
included `HostConfig.Binds=null`, `Dns=null`, `ExtraHosts=null`, `Links=null`,
and endpoint `Links=null`/`Aliases=null`, while adjacent empty fields used arrays
or objects.  These spellings all mean “no values” in Docker's create schema.

The API model now owns one null-or-value deserializer for required/default
collections.  It covers top-level labels, declared volumes, and exposed ports;
healthcheck test commands; networking endpoint maps, links, aliases, and
link-local addresses; host binds, structured mounts, tmpfs, extra hosts, DNS
lists, legacy links, and port bindings; and volume-driver options.  Missing,
`null`, and the correctly typed empty collection produce the same empty value.
Wrong non-null shapes and duplicate fields remain decoding errors.  Optional
collections retain `None` because their absence is identity-bearing rather than
an empty required/default value.

This is a bounded JSON wire-model change.  It performs no image, container,
filesystem, network, or engine operation, so no retained-C runtime domain is
implicated.  With the change, the same real Docker request advances beyond
`Binds=null` and reaches the next independent compatibility-policy gap:
`ConsoleSize=[0,0]` is currently classified as a meaningful unsupported field.
That policy domain is not changed here, and create/inspect/remove are therefore
not claimed as successful in this lane.

This matrix describes the request contract implemented by
`POST /v1.43/containers/create`. It is based on Moby 24.0.9, whose Engine API is
1.43:

- [`container.Config`](https://github.com/moby/moby/blob/v24.0.9/api/types/container/config.go)
- [`container.HostConfig`](https://github.com/moby/moby/blob/v24.0.9/api/types/container/hostconfig.go)
- [Engine API 1.43](https://docs.docker.com/reference/api/engine/version/v1.43/)

The Moby wire schema is open to additions, but Husklet must not report success
for a meaningful policy it did not apply. Unknown fields are therefore retained
during deserialization. A JSON default value (`null`, `false`, numeric zero, an
empty string, array, or object) is compatibility-inert and accepted. Every other
unknown value receives an explicit `501 Not Implemented` response. Capability
validation runs before image unpacking or network preparation, so a refused
request has no create-time side effects.

## Portable container configuration

| API 1.43 field | Classification | Husklet behavior |
|---|---|---|
| `Image` | effective | Resolves and unpacks the selected image. |
| `Entrypoint`, `Cmd`, `Env`, `WorkingDir`, `User` | effective | Override the image process configuration. Empty optional strings retain the image default. |
| `Hostname`, `Labels` | effective | Stored in the container specification. |
| `StopSignal`, `StopTimeout` | effective | Parsed into bounded lifecycle policy; timeout is limited to 86,400 seconds. |
| `Healthcheck` | effective | Parsed into the runtime health policy, or inherited from the image. |
| `Volumes`, `ExposedPorts` | effective | Create anonymous volumes and declare TCP/UDP ports; image metadata is merged. |
| `AttachStdin`, `AttachStdout`, `AttachStderr`, `OpenStdin`, `StdinOnce`, `Tty` | effective | Select the console and attachment contract. |
| `HostConfig`, `NetworkingConfig` | effective | Validated and projected as described below. |
| `Domainname`, `NetworkDisabled`, `MacAddress`, `OnBuild`, `Shell`, `ArgsEscaped` | capability refusal | Default values are inert; meaningful values receive `501`. |
| Any later portable field | capability refusal | Default values are inert; meaningful values receive `501`. |

## Host configuration

| API 1.43 field | Classification | Husklet behavior |
|---|---|---|
| `Binds` | effective subset | Absolute bind paths and named or anonymous volumes support `ro`/`rw`. Other legacy bind options refuse explicitly. |
| `Mounts` | effective subset | Bind, local volume, and tmpfs mounts are supported. Each mount validates type-specific options; unsupported propagation, recursion, drivers, and mount types refuse explicitly. |
| `Tmpfs` | effective subset | Creates an isolated tmpfs-backed volume for empty or `rw` options. Other options refuse explicitly. |
| `ExtraHosts` | effective | Validated host-to-IP entries populate the container hosts table. |
| `Memory` | effective | Nonnegative byte limit projects to runtime resources. |
| `PidsLimit` | effective | Positive limits project exactly; `null`, `0`, and `-1` mean unlimited. |
| `NanoCpus` | effective, divergent | Converts quota to a whole CPU count by rounding up. Fractional Moby quota precision is not yet preserved. |
| `ReadonlyRootfs` | effective | Selects a read-only root filesystem. |
| `NetworkMode` | effective subset | Default/bridge, none, host, and an existing named network are supported. Container-network namespace sharing is not implemented. |
| `RestartPolicy` | effective | Validated and stored as runtime restart policy. |
| `AutoRemove` | effective | Selects automatic removal after exit. |
| `PortBindings` | effective | Validated publications are applied, except host networking discards them with a create warning as Moby does. |
| `Dns`, `DnsOptions`, `DnsSearch` | effective | Validated resolver policy is persisted, projected through inspect, and used to generate the container's `/etc/resolv.conf`; see `DNS_COMPATIBILITY.md`. |
| `Links` | capability refusal | Any entry refuses explicitly; an empty list is inert. |
| `ContainerIDFile`, `LogConfig`, `VolumeDriver`, `VolumesFrom`, `ConsoleSize`, `Annotations` | capability refusal | Default values are inert; meaningful values receive `501`. |
| `CapAdd`, `CapDrop`, `CgroupnsMode`, `GroupAdd` | capability refusal | Default values are inert; meaningful values receive `501`. |
| `IpcMode`, `Cgroup`, `OomScoreAdj`, `PidMode`, `Privileged`, `PublishAllPorts` | capability refusal | Default values are inert; meaningful values receive `501`. |
| `SecurityOpt`, `StorageOpt`, `UTSMode`, `UsernsMode`, `ShmSize`, `Sysctls`, `Runtime` | capability refusal | Default values are inert; meaningful values receive `501`. |
| `Isolation`, `MaskedPaths`, `ReadonlyPaths`, `Init` | capability refusal | Default values are inert; meaningful values receive `501`. |
| `CpuShares`, `CgroupParent`, block-I/O controls, CFS/RT controls, cpusets | capability refusal | Zero/default values are inert; meaningful values receive `501`. |
| `Devices`, `DeviceCgroupRules`, `DeviceRequests`, `Ulimits` | capability refusal | Empty/default values are inert; meaningful values receive `501`. |
| `KernelMemory`, `KernelMemoryTCP`, `MemoryReservation`, `MemorySwap`, `MemorySwappiness`, `OomKillDisable` | capability refusal | Zero/default values are inert; meaningful values receive `501`. |
| Windows CPU and I/O resource controls | capability refusal | Zero/default values are inert; meaningful values receive `501`. |
| Any later host field | capability refusal | Default values are inert; meaningful values receive `501`. |

## Validation ownership

Wire-shape and capability admission belong to `hl-daemon`. Runtime-owned types
receive only settings that the daemon has already admitted. Invalid supported
values return `400 Bad Request`; valid but unavailable capabilities return `501
Not Implemented`. Validation remains duplicated at the projection boundary as a
defense against non-HTTP callers. The HTTP create preflight admits top-level and
`HostConfig` capabilities before image or network work begins; detailed validation
of admitted mount and runtime values remains with their owning projection boundaries.
