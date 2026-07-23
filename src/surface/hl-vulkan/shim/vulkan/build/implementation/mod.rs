mod extension;
mod runtime;

pub struct Implementations;

impl Implementations {
    pub fn contains(&self, name: &str) -> bool {
        runtime::NAMES.contains(&name) || extension::NAMES.contains(&name)
    }

    pub fn len(&self) -> usize {
        runtime::NAMES.len() + extension::NAMES.len()
    }
}
