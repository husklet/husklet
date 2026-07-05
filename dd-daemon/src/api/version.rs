//! `/version` DTOs.

use serde::Serialize;

// ---- /version --------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Version {
    pub version: String,
    pub api_version: &'static str,
    #[serde(rename = "MinAPIVersion")]
    pub min_api_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub kernel_version: &'static str,
    pub git_commit: &'static str,
    pub go_version: &'static str,
    pub build_time: &'static str,
    pub experimental: bool,
    pub platform: Platform,
    pub components: Vec<Component>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Platform {
    pub name: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Component {
    pub name: &'static str,
    pub version: String,
    pub details: ComponentDetails,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ComponentDetails {
    pub api_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
}
