use super::memory::ExtendedMemory;
use crate::{ExtendedClass, ExtendedReal};

impl ExtendedMemory {
    pub(super) const fn invalid_compare(class: ExtendedClass, ordered: bool) -> bool {
        matches!(class, ExtendedClass::SignalingNan | ExtendedClass::Unsupported)
            || ordered && matches!(class, ExtendedClass::QuietNan)
    }

    pub(super) fn relation(
        left: ExtendedReal,
        left_class: ExtendedClass,
        right: ExtendedReal,
        right_class: ExtendedClass,
    ) -> Option<std::cmp::Ordering> {
        if matches!(left_class, ExtendedClass::QuietNan) || matches!(right_class, ExtendedClass::QuietNan) {
            return None;
        }
        if left_class == ExtendedClass::Zero && right_class == ExtendedClass::Zero {
            return Some(std::cmp::Ordering::Equal);
        }
        let left_sign = left.bits() >> 79 != 0;
        let right_sign = right.bits() >> 79 != 0;
        if left_sign != right_sign {
            return Some(if left_sign {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            });
        }
        let magnitude = |value: ExtendedReal| ((value.bits() >> 64) as u16 & 0x7fff, value.bits() as u64);
        let order = magnitude(left).cmp(&magnitude(right));
        Some(if left_sign { order.reverse() } else { order })
    }
}
