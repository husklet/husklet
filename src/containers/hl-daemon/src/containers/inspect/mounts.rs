//! The resolved `Mounts[]` builder shared by `docker inspect` (`detail`) and `docker ps` (`list`):
//! turns a `Container`'s bind/volume/tmpfs surfaces into the JSON array a docker client reads.
use super::super::*;

/// The resolved `Mounts[]` array a docker client reads — the §6.3-9 repair. Covers ALL mount surfaces
/// with the fidelity docker reports: `-v`/Binds split into Type=bind (absolute host src) vs Type=volume
/// (a named volume, carrying Name + resolved Source mountpoint + Driver), `--mount` bind/volume specs
/// (volume ones likewise carry Name/Driver), and `--tmpfs`/`type=tmpfs` as Type=tmpfs. Previously only
/// `c.binds` were enumerated and everything was hardcoded Type=bind (Name/Driver/tmpfs all dropped).
pub(super) fn container_mounts_json(vols: &[Vol], c: &Container) -> Vec<Value> {
    let vol_mp = |name: &str| {
        vols.iter()
            .find(|v| v.name == name)
            .map(|v| v.mountpoint.clone())
            .unwrap_or_default()
    };
    let mut out: Vec<Value> = Vec::new();
    let to_val = |m: crate::api::MountPoint| serde_json::to_value(m).unwrap_or(Value::Null);
    for b in &c.binds {
        if let Some((src, dst, ro)) = parse_bind(b) {
            if src.starts_with('/') {
                out.push(to_val(crate::api::MountPoint {
                    type_: "bind".into(),
                    name: None,
                    source: src.to_string(),
                    destination: dst.to_string(),
                    driver: None,
                    mode: Some(if ro { "ro" } else { "" }.to_string()),
                    rw: !ro,
                    propagation: "rprivate",
                }));
            } else {
                out.push(to_val(crate::api::MountPoint {
                    type_: "volume".into(),
                    name: Some(src.to_string()),
                    source: vol_mp(src),
                    destination: dst.to_string(),
                    driver: Some("local"),
                    mode: Some(if ro { "ro" } else { "z" }.to_string()),
                    rw: !ro,
                    propagation: "",
                }));
            }
        }
    }
    for m in &c.mounts {
        if m.typ == "volume" {
            out.push(to_val(crate::api::MountPoint {
                type_: "volume".into(),
                name: Some(m.source.clone()),
                source: vol_mp(&m.source),
                destination: m.target.clone(),
                driver: Some("local"),
                mode: None,
                rw: !m.read_only,
                propagation: "",
            }));
        } else {
            out.push(to_val(crate::api::MountPoint {
                type_: m.typ.clone(),
                name: None,
                source: m.source.clone(),
                destination: m.target.clone(),
                driver: None,
                mode: None,
                rw: !m.read_only,
                propagation: "rprivate",
            }));
        }
    }
    for target in c.tmpfs.keys() {
        out.push(
            serde_json::to_value(crate::api::TmpfsMountPoint {
                type_: "tmpfs",
                source: "",
                destination: target.clone(),
                rw: true,
                mode: "",
            })
            .unwrap_or(Value::Null),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(name: &str, mp: &str) -> Vol {
        Vol {
            name: name.into(),
            mountpoint: mp.into(),
            created_at: 0,
            driver: "local".into(),
            options: Default::default(),
            labels: Default::default(),
        }
    }

    #[test]
    fn bind_with_absolute_source_is_type_bind() {
        let mut c = Container::default();
        c.binds = vec!["/host/data:/data".into()];
        let out = container_mounts_json(&[], &c);
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m["Type"], "bind");
        assert_eq!(m["Source"], "/host/data");
        assert_eq!(m["Destination"], "/data");
        assert_eq!(m["RW"], true);
        // A bind carries no Name/Driver (skip_serializing_if omits them).
        assert!(m.get("Name").is_none());
        assert!(m.get("Driver").is_none());
    }

    #[test]
    fn bind_with_named_volume_source_is_type_volume_with_resolved_mountpoint() {
        let mut c = Container::default();
        c.binds = vec!["myvol:/data:ro".into()];
        let vols = [vol("myvol", "/var/lib/hl/volumes/myvol")];
        let out = container_mounts_json(&vols, &c);
        let m = &out[0];
        assert_eq!(m["Type"], "volume");
        assert_eq!(m["Name"], "myvol");
        // Source is resolved to the volume's on-disk mountpoint.
        assert_eq!(m["Source"], "/var/lib/hl/volumes/myvol");
        assert_eq!(m["Driver"], "local");
        // `:ro` -> read-only.
        assert_eq!(m["RW"], false);
        assert_eq!(m["Mode"], "ro");
    }

    #[test]
    fn mount_type_volume_carries_name_and_driver() {
        let mut c = Container::default();
        c.mounts = vec![Mount {
            typ: "volume".into(),
            source: "data".into(),
            target: "/srv".into(),
            read_only: true,
            bind_options: None,
        }];
        let out = container_mounts_json(&[vol("data", "/mp/data")], &c);
        let m = &out[0];
        assert_eq!(m["Type"], "volume");
        assert_eq!(m["Name"], "data");
        assert_eq!(m["Source"], "/mp/data");
        assert_eq!(m["Driver"], "local");
        assert_eq!(m["RW"], false);
    }

    #[test]
    fn mount_type_bind_passthrough() {
        let mut c = Container::default();
        c.mounts = vec![Mount {
            typ: "bind".into(),
            source: "/h".into(),
            target: "/c".into(),
            read_only: false,
            bind_options: None,
        }];
        let out = container_mounts_json(&[], &c);
        let m = &out[0];
        assert_eq!(m["Type"], "bind");
        assert_eq!(m["Source"], "/h");
        assert_eq!(m["Destination"], "/c");
        assert_eq!(m["RW"], true);
        assert!(m.get("Name").is_none());
    }

    #[test]
    fn tmpfs_target_becomes_tmpfs_mountpoint() {
        let mut c = Container::default();
        c.tmpfs.insert("/run".into(), "size=64m".into());
        let out = container_mounts_json(&[], &c);
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m["Type"], "tmpfs");
        assert_eq!(m["Destination"], "/run");
        assert_eq!(m["RW"], true);
    }
}
