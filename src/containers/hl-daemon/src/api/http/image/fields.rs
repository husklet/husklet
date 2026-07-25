use super::{ApiError, ApiResult, BTreeMap, StatusCode};

pub(super) struct Fields<'a>(&'a BTreeMap<String, String>);

impl<'a> From<&'a BTreeMap<String, String>> for Fields<'a> {
    fn from(fields: &'a BTreeMap<String, String>) -> Self {
        Self(fields)
    }
}

impl Fields<'_> {
    pub(super) fn reject(&self, context: &str) -> ApiResult<()> {
        let Some((name, _)) = self.0.iter().find(|(_, value)| Field::meaningful(value)) else {
            return Ok(());
        };
        Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            format!("{context} option {name:?} is not implemented"),
        ))
    }
}

pub(super) struct Field<'a> {
    name: &'a str,
    value: Option<&'a str>,
}

impl<'a> Field<'a> {
    pub(super) const fn new(name: &'a str, value: Option<&'a str>) -> Self {
        Self { name, value }
    }

    pub(super) fn boolean(&self) -> ApiResult<bool> {
        match self.value.unwrap_or_default() {
            "" | "0" | "f" | "F" | "false" | "FALSE" | "False" => Ok(false),
            "1" | "t" | "T" | "true" | "TRUE" | "True" => Ok(true),
            value => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!(
                    "invalid boolean value {value:?} for image option {:?}",
                    self.name
                ),
            )),
        }
    }

    pub(super) fn meaningful(value: &str) -> bool {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "null" | "[]" | "{}"
        )
    }
}
