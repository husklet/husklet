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
                .any(|part| !matches!(part, "1" | "2" | "4" | "8"))
        {
            return Err("each work factor must be one of 1,2,4,8".into());
        }
        Ok(Self(value.into()))
    }
}
