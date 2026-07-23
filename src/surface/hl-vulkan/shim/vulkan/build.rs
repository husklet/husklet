//! Generates the complete guest Vulkan ICD export surface and resolver from the command manifest.
//!
//! Hand-written commands retain their real implementations. Every other manifest command receives
//! an ABI-correct, truthfully unsupported stub. The generated name census preserves manifest order.

#[path = "build/binding.rs"]
mod binding;
#[path = "build/implementation/mod.rs"]
mod implementation;
#[path = "build/link.rs"]
mod link;
#[path = "build/manifest.rs"]
mod manifest;
#[path = "build/staging.rs"]
mod staging;

fn main() {
    let manifest = manifest::Manifest::load();
    let implementations = implementation::Implementations;
    link::Directives::new(manifest.path()).emit();
    staging::Artifact::new(&manifest, &implementations).write();
}
