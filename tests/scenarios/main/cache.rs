use crate::fixture;
use std::env;

pub(super) fn quarantine() -> Result<(), Box<dyn std::error::Error>> {
    for reference in env::var("HL_SCENARIO_QUARANTINE")?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match fixture::quarantine(reference)? {
            Some(digest) => println!("quarantined {reference} -> {digest}; CAS retained"),
            None => println!("no named metadata for {reference}"),
        }
    }
    Ok(())
}
