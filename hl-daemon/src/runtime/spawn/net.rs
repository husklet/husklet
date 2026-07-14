use super::*;

/// Write the LIVE reach-by-name table for one user-defined network into the engine's per-network switch
/// dir (`/tmp/.hlbr-<netid[..40]>/.names`), one `ip\tname` line per endpoint. The in-engine 127.0.0.11
/// resolver reads this file per DNS query (net.c `dns_local_lookup`) BEFORE falling through to the macOS
/// host resolver, so a container resolves a same-network peer by name even if that peer joined AFTER this
/// container launched (its `/etc/hosts` snapshot, seeded once at start, can't see it). The `.40s`
/// truncation matches the engine's `snprintf` for `HL_NETBR`, so the path byte-matches what the engine
/// computes. Best-effort: never fail a spawn on an I/O error.
pub(crate) fn write_net_names(netid: &str, endpoints: &HashMap<String, Endpoint>) {
    let dir = format!("/tmp/.hlbr-{}", &netid[..netid.len().min(40)]);
    let _ = std::fs::create_dir_all(&dir); // the engine also mkdir 0700's this; either creating it is fine
    let mut body = String::new();
    for e in endpoints.values() {
        if !e.ip.is_empty() && !e.name.is_empty() {
            body.push_str(&format!("{}\t{}\n", e.ip, e.name));
        }
    }
    let _ = std::fs::write(format!("{dir}/.names"), body);
}
