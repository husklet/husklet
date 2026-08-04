use crate::ScalarWidth;

pub(crate) struct Extension;

impl Extension {
    pub(crate) const fn sign(value: u64, source: ScalarWidth) -> Option<u64> {
        match source {
            ScalarWidth::Byte => Some((value as i8) as i64 as u64),
            ScalarWidth::Word => Some((value as i16) as i64 as u64),
            ScalarWidth::Dword => Some((value as i32) as i64 as u64),
            ScalarWidth::Qword => None,
        }
    }
}
