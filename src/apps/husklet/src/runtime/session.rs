//! Linux identity used by interactive processes in one workspace.

use hl_container::{Containers, Limits, Process};
use hl_images::{Images, UnpackedImage};
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

pub(super) const USER_LABEL: &str = "husklet.workspace.session.user";
pub(super) const HOME_LABEL: &str = "husklet.workspace.session.home";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Session {
    user: String,
    home: String,
    provision: Option<Provision>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Provision {
    uid: u32,
    gid: u32,
    passwd: String,
    group: String,
}

impl Session {
    pub(super) fn select(images: &Images, image: &UnpackedImage) -> io::Result<Self> {
        let reference = images.rootfs(image).map_err(io::Error::other)?;
        let selected = images
            .roots()
            .open(&reference)
            .map_err(io::Error::other)
            .and_then(|root| Self::from_root(image.runtime().user.as_str(), root.path()));
        let released = images.roots().release(&reference).map_err(io::Error::other);
        match (selected, released) {
            (Ok(session), Ok(())) => Ok(session),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(select), Err(release)) => Err(io::Error::other(format!(
                "{select}; temporary image root release failed: {release}"
            ))),
        }
    }

    fn from_root(image_user: &str, root: &Path) -> io::Result<Self> {
        let passwd = Self::read_optional(root.join("etc/passwd"))?;
        let group = Self::read_optional(root.join("etc/group"))?;
        let accounts = Accounts::parse(&passwd);

        if !image_user.is_empty() {
            let (uid, gid) = Process::resolve_user(image_user, root).map_err(io::Error::other)?;
            if uid != 0 {
                let uid = u32::try_from(uid).map_err(|_| io::Error::other("image user has a negative UID"))?;
                let gid = u32::try_from(gid).map_err(|_| io::Error::other("image user has a negative GID"))?;
                let home = accounts
                    .iter()
                    .find(|account| account.uid == uid)
                    .map_or("/tmp", |account| account.home.as_str());
                return Ok(Self::existing(uid, gid, home));
            }
        }

        if let Some(account) = accounts.iter().find(|account| account.is_interactive(root)) {
            return Ok(Self::existing(account.uid, account.gid, &account.home));
        }

        let used_uids = accounts.iter().map(|account| account.uid).collect::<BTreeSet<_>>();
        let used_gids = Groups::ids(&group);
        let id = (1000..65534)
            .find(|id| !used_uids.contains(id) && !used_gids.contains(id))
            .ok_or_else(|| io::Error::other("image has no available regular Linux account ID"))?;
        let name = if accounts.iter().any(|account| account.name == "husklet") {
            format!("husklet-{id}")
        } else {
            "husklet".into()
        };
        let home = "/home/husklet";
        Ok(Self {
            user: format!("{id}:{id}"),
            home: home.into(),
            provision: Some(Provision {
                uid: id,
                gid: id,
                passwd: Self::append_line(passwd, &format!("{name}:x:{id}:{id}:Husklet Workspace:{home}:/bin/sh")),
                group: Self::append_line(group, &format!("{name}:x:{id}:")),
            }),
        })
    }

    fn read_optional(path: impl AsRef<Path>) -> io::Result<String> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error),
        }
    }

    fn existing(uid: u32, gid: u32, home: &str) -> Self {
        Self {
            user: format!("{uid}:{gid}"),
            home: if home.starts_with('/') && !home.is_empty() {
                home.into()
            } else {
                "/tmp".into()
            },
            provision: None,
        }
    }

    fn append_line(mut contents: String, line: &str) -> String {
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(line);
        contents.push('\n');
        contents
    }

    pub(super) fn from_labels(labels: &std::collections::BTreeMap<String, String>) -> io::Result<Self> {
        let user = labels
            .get(USER_LABEL)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| io::Error::other("workspace container has no session user"))?;
        let home = labels
            .get(HOME_LABEL)
            .filter(|value| value.starts_with('/'))
            .ok_or_else(|| io::Error::other("workspace container has no valid session home"))?;
        Ok(Self {
            user: user.clone(),
            home: home.clone(),
            provision: None,
        })
    }

    pub(super) fn user(&self) -> &str {
        &self.user
    }

    pub(super) fn home(&self) -> &str {
        &self.home
    }

    pub(super) fn label(&self, spec: hl_container::ContainerSpec) -> hl_container::ContainerSpec {
        spec.label(USER_LABEL, &self.user).label(HOME_LABEL, &self.home)
    }

    pub(super) async fn provision(&self, containers: &Containers) -> io::Result<()> {
        let Some(provision) = &self.provision else {
            return Ok(());
        };
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            Self::file(&mut archive, "etc/passwd", provision.passwd.as_bytes(), 0, 0)?;
            Self::file(&mut archive, "etc/group", provision.group.as_bytes(), 0, 0)?;
            Self::directory(
                &mut archive,
                self.home.trim_start_matches('/'),
                provision.uid,
                provision.gid,
            )?;
            archive.finish()?;
        }
        containers
            .filesystem("workspace")
            .await
            .map_err(io::Error::other)?
            .extract_owned("/", bytes.as_slice(), Limits::default(), true)
            .map_err(io::Error::other)
    }

    fn file(
        archive: &mut tar::Builder<&mut Vec<u8>>,
        path: &str,
        contents: &[u8],
        uid: u32,
        gid: u32,
    ) -> io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_uid(u64::from(uid));
        header.set_gid(u64::from(gid));
        header.set_size(contents.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, path, contents)
    }

    fn directory(archive: &mut tar::Builder<&mut Vec<u8>>, path: &str, uid: u32, gid: u32) -> io::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_uid(u64::from(uid));
        header.set_gid(u64::from(gid));
        header.set_size(0);
        header.set_cksum();
        archive.append_data(&mut header, path, io::empty())
    }
}

#[derive(Clone, Debug)]
struct Account {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
    shell: String,
}

impl Account {
    fn is_interactive(&self, root: &Path) -> bool {
        (1000..65534).contains(&self.uid)
            && self.home.starts_with('/')
            && self.home != "/"
            && root.join(self.home.trim_start_matches('/')).is_dir()
            && !self.shell.ends_with("/false")
            && !self.shell.ends_with("/nologin")
    }
}

struct Accounts;

impl Accounts {
    fn parse(passwd: &str) -> Vec<Account> {
        passwd
            .lines()
            .filter_map(|line| {
                let fields = line.split(':').collect::<Vec<_>>();
                if fields.len() < 7 {
                    return None;
                }
                Some(Account {
                    name: fields[0].into(),
                    uid: fields[2].parse().ok()?,
                    gid: fields[3].parse().ok()?,
                    home: fields[5].into(),
                    shell: fields[6].into(),
                })
            })
            .collect()
    }
}

struct Groups;

impl Groups {
    fn ids(group: &str) -> BTreeSet<u32> {
        group
            .lines()
            .filter_map(|line| line.split(':').nth(2)?.parse().ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn provisions_first_free_regular_identity_when_an_image_has_only_system_accounts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("etc")).unwrap();
        std::fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nsystem:x:999:999::/:/bin/false\n",
        )
        .unwrap();
        std::fs::write(root.path().join("etc/group"), "root:x:0:\ntaken:x:1000:\n").unwrap();

        let session = Session::from_root("", root.path()).unwrap();

        assert_eq!(session.user(), "1001:1001");
        assert_eq!(session.home(), "/home/husklet");
        let provision = session.provision.unwrap();
        assert!(provision.passwd.contains("husklet:x:1001:1001:"));
        assert!(provision.group.contains("husklet:x:1001:"));
    }

    #[test]
    fn honors_an_existing_non_root_image_user_and_home() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("etc")).unwrap();
        std::fs::write(
            root.path().join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nubuntu:x:1000:1002::/home/ubuntu:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(root.path().join("etc/group"), "ubuntu:x:1002:\n").unwrap();
        std::fs::create_dir_all(root.path().join("home/ubuntu")).unwrap();

        let session = Session::from_root("ubuntu", root.path()).unwrap();

        assert_eq!(session.user(), "1000:1002");
        assert_eq!(session.home(), "/home/ubuntu");
        assert!(session.provision.is_none());
    }

    #[test]
    fn provisions_minimal_images_without_account_databases() {
        let root = tempfile::tempdir().unwrap();

        let session = Session::from_root("", root.path()).unwrap();

        assert_eq!(session.user(), "1000:1000");
        assert!(session.provision.unwrap().passwd.starts_with("husklet:x:1000:1000:"));
    }
}
