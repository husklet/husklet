//! Typed image-command contracts.

use crate::contract::{Group, Scenario, Target};

fn host(id: &'static str, command: &str) -> Scenario {
    Scenario::new(id, "alpine:latest")
        .only(&[Target::Arm64])
        .timeout(30)
        .host(command)
}

pub fn group() -> Group {
    Group::new("imagescmd", vec![
        host("imagescmd/list", "docker images --format \"{{.Repository}}:{{.Tag}}\" | grep -q \"^alpine:latest\" && echo ALPINE_LISTED").contains("ALPINE_LISTED"),
        host("imagescmd/tag", "docker tag alpine:latest ${C}img:v1\ndocker images --format \"{{.Repository}}:{{.Tag}}\" | grep -q \"${C}img:v1\" && echo TAGGED\ndocker rmi ${C}img:v1 >/dev/null 2>&1").contains("TAGGED"),
        host("imagescmd/rmi", "docker tag alpine:latest ${C}img:rmv\ndocker rmi ${C}img:rmv >/dev/null\ndocker images --format \"{{.Repository}}:{{.Tag}}\" | grep -q \"${C}img:rmv\" && echo STILL_THERE || echo RMI_OK").contains("RMI_OK"),
        host("imagescmd/history", "docker history --no-trunc alpine:latest >/dev/null 2>&1 && echo HISTORY_OK").contains("HISTORY_OK"),
        host("imagescmd/inspect", "docker image inspect -f \"{{.Os}}/{{.Architecture}}\" alpine:latest").contains("linux/"),
    ])
}
