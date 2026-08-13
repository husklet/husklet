use std::str::FromStr;

#[derive(Clone)]
pub(super) struct WorkFactor(pub(super) String);

impl WorkFactor {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for WorkFactor {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((first, second)) = value.split_once(',') else {
            return Err("work factor must be a pair such as 4,2".into());
        };
        if second.contains(',')
            || [first, second]
                .into_iter()
                .any(|part| !matches!(part, "1" | "2" | "4" | "8" | "16" | "32" | "64" | "128"))
        {
            return Err("each work factor must be one of 1,2,4,8,16,32,64,128".into());
        }
        Ok(Self(value.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::WorkFactor;

    #[test]
    fn accepts_only_canonical_bounded_factor_pairs() {
        for value in ["1,2", "4,8", "16,32", "64,128"] {
            assert_eq!(value.parse::<WorkFactor>().unwrap().as_str(), value);
        }
        for value in [
            "", "1", "1,", ",1", "1,2,4", "0,1", "3,4", "256,1", "01,2", "+1,2", " 1,2", "1,2 ",
        ] {
            assert!(value.parse::<WorkFactor>().is_err(), "accepted {value:?}");
        }
    }
}
