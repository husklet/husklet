use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_linux::{OpenAbiPlan, PathOperand, ResolveFlags};
use hl_runtime::{GuestPathBytes, OpenDirectory, OpenIntent, RuntimePathError, RuntimePathHost};

use super::{NativePath, watch};

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: std::path::PathBuf,
    tasks: Arc<hl_task::TaskRegistry>,
    parent: hl_task::ProcessId,
    child: hl_task::ProcessId,
    host: Arc<NativePath>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hl-proc-link-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("work")).unwrap();
        let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
        let credentials = hl_task::ProcessCredentials::new(0, 0, &[], 16).unwrap();
        let (parent, parent_thread) = tasks.create_init(credentials, hl_task::ProcessLimits::empty()).unwrap();
        let (child, child_thread) = tasks
            .commit_fork_process(tasks.begin_fork_process(parent_thread).unwrap())
            .unwrap();
        let bytes = root.to_str().unwrap().as_bytes();
        let base = NativePath::new(bytes, watch::Hub::new(bytes).unwrap())
            .unwrap()
            .with_process(
                Arc::clone(&tasks),
                parent,
                Arc::new(hl_runtime::NamespaceHandleRegistry::new()),
                Arc::new(hl_descriptor::DescriptorTable::new(16).unwrap()),
            );
        let working = Arc::new(hl_runtime::WorkingDirectory::root());
        working.replace_path("/work").unwrap();
        let host = base.for_test(
            child,
            child_thread,
            Arc::new(hl_descriptor::DescriptorTable::new(16).unwrap()),
            working,
            Arc::new(hl_runtime::FsContext::default()),
        );
        Self {
            root,
            tasks,
            parent,
            child,
            host,
        }
    }

    fn operand(path: &[u8], nofollow: bool) -> PathOperand {
        PathOperand {
            directory: OpenDirectory::default(),
            path: GuestPathBytes::new(path).unwrap(),
            allow_empty: false,
            nofollow,
        }
    }

    fn plan(path: &[u8], intent: u32, nofollow: bool) -> OpenAbiPlan {
        OpenAbiPlan {
            operand: Self::operand(path, nofollow),
            intent: OpenIntent::from_bits(intent),
            mode: 0,
            close_on_exec: false,
            nonblocking: false,
            no_controlling_terminal: false,
            resolve: ResolveFlags::default(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn magic_link_follow() {
    let fixture = Fixture::new();
    let base = fixture.host.root_base().unwrap();
    for (path, target) in [
        (b"/proc/self/root".as_slice(), b"/".as_slice()),
        (b"/proc/self/cwd", b"/work"),
    ] {
        let link = fixture.host.resolve(&base, &Fixture::operand(path, true)).unwrap();
        assert_eq!(link.read_link().unwrap(), target);
        assert_eq!(link.metadata().unwrap().kind, hl_runtime::FileKind::Symlink);

        let followed = fixture.host.resolve(&base, &Fixture::operand(path, false)).unwrap();
        assert_eq!(followed.metadata().unwrap().kind, hl_runtime::FileKind::Directory);
        let opened = fixture
            .host
            .prepare_open(
                &base,
                &Fixture::plan(path, OpenIntent::READ | OpenIntent::DIRECTORY, false),
            )
            .unwrap();
        assert_eq!(opened.object().metadata().unwrap().kind, 4);
    }
}

#[test]
fn magic_link_nofollow() {
    let fixture = Fixture::new();
    let base = fixture.host.root_base().unwrap();
    let plan = Fixture::plan(b"/proc/self/cwd", OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW, true);
    let opened = fixture.host.prepare_open(&base, &plan).unwrap();
    assert_eq!(opened.object().metadata().unwrap().kind, 10);

    let directory = Fixture::plan(
        b"/proc/self/cwd",
        OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW | OpenIntent::DIRECTORY,
        true,
    );
    assert_eq!(
        fixture.host.prepare_open(&base, &directory).unwrap_err(),
        RuntimePathError::NotDirectory,
    );
    assert_eq!(
        fixture
            .host
            .prepare_open(&base, &Fixture::plan(b"/proc/self/cwd", OpenIntent::READ, true))
            .unwrap_err(),
        RuntimePathError::Loop,
    );
    let mut no_magic = Fixture::plan(b"/proc/self/cwd", OpenIntent::READ, false);
    no_magic.resolve.no_magic_links = true;
    assert_eq!(
        fixture.host.prepare_open(&base, &no_magic).unwrap_err(),
        RuntimePathError::Loop,
    );
}

#[test]
fn stale_magic_link() {
    let fixture = Fixture::new();
    fixture
        .tasks
        .exit_process(fixture.child, hl_task::ExitStatus::Code(0))
        .unwrap();
    fixture.tasks.reap(fixture.parent, fixture.child).unwrap();
    let base = fixture.host.root_base().unwrap();
    assert_eq!(
        fixture
            .host
            .resolve(&base, &Fixture::operand(b"/proc/self/root", false))
            .unwrap_err(),
        RuntimePathError::NotFound,
    );
}
