//! ANGLE-style GLSL-ES corpus cases grouped by shader capability.

use super::{fs, vs, Case, NagaLimit, Pass};

#[path = "glsl_es_corpus_data/advanced.rs"]
mod advanced;
#[path = "glsl_es_corpus_data/interface.rs"]
mod interface;
#[path = "glsl_es_corpus_data/language.rs"]
mod language;
#[path = "glsl_es_corpus_data/profile.rs"]
mod profile;
#[path = "glsl_es_corpus_data/texture.rs"]
mod texture;

pub(super) const GROUPS: &[&[Case]] = &[
    profile::CASES,
    interface::CASES,
    texture::CASES,
    language::CASES,
    advanced::CASES,
];
