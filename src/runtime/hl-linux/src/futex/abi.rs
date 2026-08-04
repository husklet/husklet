use hl_sync::{FutexAtomicOperation, FutexClock, FutexDeadline};

use crate::{FutexMarshalError, GuestMemory, TimeFutexAbi};

const FUTEX_WAIT_VECTOR_MAXIMUM: usize = 128;
const FUTEX_WAIT_VECTOR_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Wait,
    Wake,
    FileDescriptor,
    Requeue,
    CompareRequeue,
    WakeOperation(FutexAtomicOperation),
    LockPriorityInheritance,
    UnlockPriorityInheritance,
    TryLockPriorityInheritance,
    WaitBitset,
    WakeBitset,
    WaitRequeuePriorityInheritance,
    CompareRequeuePriorityInheritance,
    LockPriorityInheritance2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Plan {
    pub operation: Operation,
    pub address: u64,
    pub private: bool,
    pub value: u32,
    pub secondary_address: u64,
    pub secondary_count: u32,
    pub secondary_value: u32,
    pub bitset: u32,
    pub deadline: Option<FutexDeadline>,
    pub timeout_absolute: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitVector {
    pub value: u64,
    pub address: u64,
    pub private: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RobustListPlan {
    pub head: u64,
    pub length: usize,
}

impl<M: GuestMemory> TimeFutexAbi<'_, M> {
    pub fn futex(
        &self,
        address: u64,
        encoded_operation: u32,
        value: u32,
        timeout_or_value: u64,
        secondary_address: u64,
        secondary_value: u32,
    ) -> Result<Plan, FutexMarshalError> {
        Self::aligned(address)?;
        let private = encoded_operation & 128 != 0;
        let realtime = encoded_operation & 256 != 0;
        let operation = Self::decode_operation(encoded_operation & 127, secondary_value)?;
        if matches!(operation, Operation::WakeOperation(_)) && ((secondary_value >> 24) & 15) > 5 {
            return Err(FutexMarshalError::Invalid);
        }
        if realtime
            && !matches!(
                operation,
                Operation::WaitBitset | Operation::WaitRequeuePriorityInheritance | Operation::LockPriorityInheritance2
            )
        {
            return Err(FutexMarshalError::Invalid);
        }
        Self::validate_counts(operation, value, timeout_or_value as u32)?;
        let uses_secondary = matches!(
            operation,
            Operation::Requeue
                | Operation::CompareRequeue
                | Operation::WakeOperation(_)
                | Operation::WaitRequeuePriorityInheritance
                | Operation::CompareRequeuePriorityInheritance
        );
        if uses_secondary && secondary_address != 0 {
            Self::aligned(secondary_address)?;
        }
        let timed = matches!(
            operation,
            Operation::Wait
                | Operation::WaitBitset
                | Operation::WaitRequeuePriorityInheritance
                | Operation::LockPriorityInheritance
                | Operation::LockPriorityInheritance2
        );
        let deadline = if timed && timeout_or_value != 0 {
            let value = self.timespec(timeout_or_value)?;
            Some(FutexDeadline {
                clock: if matches!(operation, Operation::LockPriorityInheritance) || realtime {
                    FutexClock::Realtime
                } else {
                    FutexClock::Monotonic
                },
                value,
            })
        } else {
            None
        };
        Ok(Plan {
            operation,
            address,
            private,
            value,
            secondary_address,
            secondary_count: timeout_or_value as u32,
            secondary_value,
            bitset: if matches!(operation, Operation::WaitBitset | Operation::WakeBitset) {
                secondary_value
            } else {
                u32::MAX
            },
            deadline,
            timeout_absolute: !matches!(operation, Operation::Wait),
        })
    }

    pub fn wait_vectors(
        &self,
        address: u64,
        count: usize,
        flags: u32,
        timeout: u64,
        clock: i32,
    ) -> Result<(Vec<WaitVector>, Option<FutexDeadline>), FutexMarshalError> {
        if count == 0 || count > FUTEX_WAIT_VECTOR_MAXIMUM || flags != 0 {
            return Err(FutexMarshalError::Invalid);
        }
        let clock = match clock {
            0 => FutexClock::Realtime,
            1 => FutexClock::Monotonic,
            _ => return Err(FutexMarshalError::Invalid),
        };
        let length = count
            .checked_mul(FUTEX_WAIT_VECTOR_SIZE)
            .ok_or(FutexMarshalError::Overflow)?;
        let mut bytes = vec![0; length];
        if self.marshaller.copy_from(address, &mut bytes).fault.is_some() {
            return Err(FutexMarshalError::Fault);
        }
        let vectors = bytes
            .chunks_exact(FUTEX_WAIT_VECTOR_SIZE)
            .map(Self::wait_vector)
            .collect::<Result<Vec<_>, _>>()?;
        let deadline = (timeout != 0)
            .then(|| self.timespec(timeout).map(|value| FutexDeadline { clock, value }))
            .transpose()?;
        Ok((vectors, deadline))
    }

    pub fn robust_list(&self, head: u64, length: usize) -> Result<RobustListPlan, FutexMarshalError> {
        if length != 24 {
            return Err(FutexMarshalError::Invalid);
        }
        Ok(RobustListPlan { head, length })
    }

    fn decode_operation(raw: u32, encoded: u32) -> Result<Operation, FutexMarshalError> {
        match raw {
            0 => Ok(Operation::Wait),
            1 => Ok(Operation::Wake),
            2 => Ok(Operation::FileDescriptor),
            3 => Ok(Operation::Requeue),
            4 => Ok(Operation::CompareRequeue),
            5 => Ok(Operation::WakeOperation(Self::atomic_operation(encoded)?)),
            6 => Ok(Operation::LockPriorityInheritance),
            7 => Ok(Operation::UnlockPriorityInheritance),
            8 => Ok(Operation::TryLockPriorityInheritance),
            9 => Ok(Operation::WaitBitset),
            10 => Ok(Operation::WakeBitset),
            11 => Ok(Operation::WaitRequeuePriorityInheritance),
            12 => Ok(Operation::CompareRequeuePriorityInheritance),
            13 => Ok(Operation::LockPriorityInheritance2),
            _ => Err(FutexMarshalError::Invalid),
        }
    }

    const fn validate_counts(operation: Operation, primary: u32, secondary: u32) -> Result<(), FutexMarshalError> {
        if matches!(operation, Operation::CompareRequeuePriorityInheritance) && primary != 1 {
            return Err(FutexMarshalError::Invalid);
        }
        let primary_count = matches!(
            operation,
            Operation::Wake
                | Operation::WakeBitset
                | Operation::Requeue
                | Operation::CompareRequeue
                | Operation::WakeOperation(_)
                | Operation::CompareRequeuePriorityInheritance
        );
        let secondary_count = matches!(
            operation,
            Operation::Requeue
                | Operation::CompareRequeue
                | Operation::WakeOperation(_)
                | Operation::CompareRequeuePriorityInheritance
        );
        if (primary_count && (primary as i32) < 0) || (secondary_count && (secondary as i32) < 0) {
            Err(FutexMarshalError::Invalid)
        } else {
            Ok(())
        }
    }

    fn atomic_operation(encoded: u32) -> Result<FutexAtomicOperation, FutexMarshalError> {
        let encoded_operation = encoded >> 28;
        let mut argument = (((encoded >> 12) << 20) as i32) >> 20;
        let operation = encoded_operation & 7;
        if encoded_operation & 8 != 0 {
            if !(0..32).contains(&argument) {
                return Err(FutexMarshalError::Invalid);
            }
            argument = 1_i32.wrapping_shl(argument as u32);
        }
        match operation {
            0 => Ok(FutexAtomicOperation::Set(argument)),
            1 => Ok(FutexAtomicOperation::Add(argument)),
            2 => Ok(FutexAtomicOperation::Or(argument)),
            3 => Ok(FutexAtomicOperation::AndNot(argument)),
            4 => Ok(FutexAtomicOperation::Xor(argument)),
            _ => Err(FutexMarshalError::Invalid),
        }
    }

    fn wait_vector(bytes: &[u8]) -> Result<WaitVector, FutexMarshalError> {
        let value = Self::word(bytes, 0);
        let address = Self::word(bytes, 8);
        let flags = Self::unsigned(bytes, 16);
        if Self::unsigned(bytes, 20) != 0 || flags & !130 != 0 || flags & 2 == 0 {
            return Err(FutexMarshalError::Invalid);
        }
        Self::aligned(address)?;
        Ok(WaitVector {
            value,
            address,
            private: flags & 128 != 0,
        })
    }

    const fn aligned(address: u64) -> Result<(), FutexMarshalError> {
        if address & 3 == 0 {
            Ok(())
        } else {
            Err(FutexMarshalError::Invalid)
        }
    }

    fn unsigned(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("unsigned"))
    }
}

#[cfg(test)]
mod tests {
    use hl_isa::GuestArchitecture;

    use super::Operation;
    use crate::{FutexMarshalError, GuestAccess, GuestFault, GuestMemory, TimeFutexAbi};

    const BASE: u64 = 0x1000;

    struct Memory;

    struct DeadlineMemory;

    impl GuestMemory for Memory {
        fn probe(&self, _: u64, _: usize, _: GuestAccess) -> Result<usize, GuestFault> {
            Ok(0)
        }

        fn read(&self, address: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
            Err(GuestFault {
                address,
                access: GuestAccess::Read,
            })
        }

        fn write(&self, _: u64, _: &[u8]) -> Result<usize, GuestFault> {
            Ok(0)
        }
    }

    impl GuestMemory for DeadlineMemory {
        fn probe(&self, _: u64, _: usize, _: GuestAccess) -> Result<usize, GuestFault> {
            Ok(0)
        }

        fn read(&self, _: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
            let mut timespec = [0_u8; 16];
            timespec[..8].copy_from_slice(&1_i64.to_le_bytes());
            timespec[8..].copy_from_slice(&2_i64.to_le_bytes());
            output.copy_from_slice(&timespec[..output.len()]);
            Ok(output.len())
        }

        fn write(&self, _: u64, _: &[u8]) -> Result<usize, GuestFault> {
            Ok(0)
        }
    }

    #[test]
    fn secondary_argument_order() {
        for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
            let abi = TimeFutexAbi::new(&Memory, architecture);
            let wake = abi.futex(BASE, 1, 1, 0, BASE + 1, 0).unwrap();
            assert_eq!(wake.operation, Operation::Wake);
            assert_eq!(wake.secondary_address, BASE + 1);
            assert_eq!(
                abi.futex(BASE, 11, 0, u64::MAX, BASE + 1, 0),
                Err(FutexMarshalError::Invalid),
            );
        }
    }

    #[test]
    fn signed_counts_rejected() {
        for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
            let abi = TimeFutexAbi::new(&Memory, architecture);
            for operation in [1, 10] {
                assert_eq!(
                    abi.futex(BASE, operation, u32::MAX, 0, 0, u32::MAX),
                    Err(FutexMarshalError::Invalid),
                );
            }
            for operation in [3, 4, 5, 12] {
                assert_eq!(
                    abi.futex(BASE, operation, 1, u32::MAX as u64, BASE, 0),
                    Err(FutexMarshalError::Invalid),
                );
            }
            assert!(abi.futex(BASE, 1, i32::MAX as u32, 0, 0, 0).is_ok());
            assert_eq!(abi.futex(BASE, 12, 0, 1, BASE, 0), Err(FutexMarshalError::Invalid));
            assert_eq!(abi.futex(BASE, 12, 2, 1, BASE, 0), Err(FutexMarshalError::Invalid));
            assert!(abi.futex(BASE, 12, 1, 1, BASE, 0).is_ok());
        }
    }

    #[test]
    fn zero_bitset_deferred() {
        for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
            let abi = TimeFutexAbi::new(&Memory, architecture);
            let plan = abi.futex(BASE, 10, 1, 0, 0, 0).unwrap();
            assert_eq!(plan.operation, Operation::WakeBitset);
            assert_eq!(plan.bitset, 0);
        }
    }

    #[test]
    fn pi2_clock_selection() {
        for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
            let abi = TimeFutexAbi::new(&DeadlineMemory, architecture);
            let monotonic = abi.futex(BASE, 13, 0, BASE + 8, 0, 0).unwrap();
            let realtime = abi.futex(BASE, 13 | 256, 0, BASE + 8, 0, 0).unwrap();
            assert_eq!(monotonic.deadline.unwrap().clock, hl_sync::FutexClock::Monotonic);
            assert_eq!(realtime.deadline.unwrap().clock, hl_sync::FutexClock::Realtime);
        }
    }
}
