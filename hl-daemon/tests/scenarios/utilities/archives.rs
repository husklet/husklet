//! tar / gzip round-trips — create/extract, content verified after extract.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- tar / gzip round-trips --------------------------------------------------------------
        scen("utilities/tar-roundtrip", "alpine")
            .exec("cd /tmp && rm -rf d a.tar && mkdir d && echo dd-tar-ok > d/f.txt && tar cf a.tar d && rm -rf d && tar xf a.tar && cat d/f.txt")
            .has("dd-tar-ok"),
        scen("utilities/gzip-roundtrip", "alpine")
            .exec("printf 'dd-gzip-ok\\n' | gzip | gunzip")
            .has("dd-gzip-ok"),
        // tar+gzip combined, content checked via awk sum after extract.
        scen("utilities/targz-roundtrip", "alpine")
            .exec("cd /tmp && rm -rf g g.tgz && mkdir g && seq 1 1000 > g/n.txt && tar czf g.tgz g && rm -rf g && tar xzf g.tgz && awk '{s+=$1}END{print s}' g/n.txt")
            .has("500500"),
        scen("utilities/tar-roundtrip-glibc", "debian:bookworm")
            .exec("cd /tmp && rm -rf d a.tar && mkdir d && echo dd-tar-ok > d/f.txt && tar cf a.tar d && rm -rf d && tar xf a.tar && cat d/f.txt")
            .has("dd-tar-ok"),
        scen("utilities/gzip-roundtrip-glibc", "debian:bookworm")
            .exec("printf 'dd-gzip-ok\\n' | gzip | gunzip")
            .has("dd-gzip-ok"),
    ]
}
