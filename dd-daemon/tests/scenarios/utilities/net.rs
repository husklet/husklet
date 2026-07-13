//! curl / socat / loopback networking — hermetic banners + 127.0.0.1 round-trips.

use crate::scenario::{scen, Scenario};

pub(super) fn items() -> Vec<Scenario> {
    vec![
        // ---- curl / socat / loopback networking --------------------------------------------------
        // hermetic: banner only (no network round-trip).
        scen("utilities/curl-version", "curlimages/curl:latest")
            .run(&["--version"])
            .has("curl 8."),
        scen("utilities/socat-version", "alpine/socat:latest")
            .run(&["-V"])
            .has("socat by Gerhard Rieger"),
        // loopback TCP echo via busybox nc — fork + 127.0.0.1 round-trip, fully hermetic.
        scen("utilities/nc-loopback", "alpine")
            .exec("{ echo dd-echo-ok | nc -l -p 9000; } & sleep 0.4; nc 127.0.0.1 9000 </dev/null")
            .has("dd-echo-ok"),
        // loopback HTTP: busybox httpd serves a file, wget fetches it (server+client fork, no network).
        scen("utilities/wget-loopback", "busybox:latest")
            .exec("mkdir -p /www && echo dd-http-ok > /www/f.txt && httpd -p 127.0.0.1:8080 -h /www && sleep 0.3 && wget -qO- http://127.0.0.1:8080/f.txt")
            .has("dd-http-ok"),
    ]
}
