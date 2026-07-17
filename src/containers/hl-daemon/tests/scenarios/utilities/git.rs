//! git — alpine/git (entrypoint=git → run banner) + bitnami/git (passthrough → exec workflow).

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- git (alpine/git entrypoint=git → run banner; bitnami/git passthrough → exec workflow) -
        scen("utilities/git-version", "alpine/git:latest")
            .run(&["--version"])
            .has("git version 2."),
        // canonical empty-blob SHA (well-known, version-independent).
        scen("utilities/git-empty-blob", "bitnami/git:latest")
            .exec("printf '' | git hash-object --stdin")
            .has("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
        // blob of "hl\n" — verified+pinned on the Real oracle.
        scen("utilities/git-hashobject-dd", "bitnami/git:latest")
            .exec("printf 'dd\\n' | git hash-object --stdin")
            .has("f03f6945fbf941fa91cb460eab583c7f36c8cee3"),
        // init + add + commit + log — fixed identity so the message is stable.
        scen("utilities/git-init-commit", "bitnami/git:latest")
            .exec("export GIT_AUTHOR_NAME=dd GIT_AUTHOR_EMAIL=dd@dd GIT_COMMITTER_NAME=dd GIT_COMMITTER_EMAIL=dd@dd; \
                   export GIT_AUTHOR_DATE='2000-01-01T00:00:00Z' GIT_COMMITTER_DATE='2000-01-01T00:00:00Z'; \
                   cd /tmp && rm -rf r && mkdir r && cd r && git init -q && echo dd > f && git add f && \
                   git commit -q -m 'dd: first commit' && git log --format='%s' -1")
            .has("dd: first commit"),
        // fully deterministic commit SHA: every input (tree, identity, dates, message) pinned.
        scen("utilities/git-deterministic-sha", "bitnami/git:latest")
            .exec("export GIT_AUTHOR_NAME=dd GIT_AUTHOR_EMAIL=dd@dd GIT_COMMITTER_NAME=dd GIT_COMMITTER_EMAIL=dd@dd; \
                   export GIT_AUTHOR_DATE='2000-01-01T00:00:00Z' GIT_COMMITTER_DATE='2000-01-01T00:00:00Z'; \
                   cd /tmp && rm -rf r && mkdir r && cd r && git init -q && echo dd > f && git add f && \
                   git commit -q -m 'dd: first commit' && git rev-parse HEAD")
            .has("9fba1c3dda82182611817eab9c713c8f5afbd0c1"),
    ]
}
