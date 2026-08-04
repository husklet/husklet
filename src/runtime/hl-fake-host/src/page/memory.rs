use hl_execution::{FetchError, GuestOperandMemory, InstructionFetch};
use hl_linux::{GuestAccess, GuestFault, GuestMemory};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

pub const PAGE_SIZE: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Protection(u8);

impl Protection {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);
    pub const READ_WRITE: Self = Self(3);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn permits(self, access: GuestAccess) -> bool {
        match access {
            GuestAccess::Read => self.0 & Self::READ.0 != 0,
            GuestAccess::Write => self.0 & Self::WRITE.0 != 0,
        }
    }
}

struct Page {
    bytes: Arc<RwLock<Vec<u8>>>,
    protection: Protection,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteReservation {
    address: u64,
    bytes: u8,
}

#[derive(Default)]
pub struct GuestPageStore {
    pages: Mutex<BTreeMap<u64, Page>>,
    next_generation: Mutex<u64>,
}

impl GuestPageStore {
    pub fn map(&self, address: u64, protection: Protection) -> Result<u64, GuestFault> {
        if address % PAGE_SIZE != 0 {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let mut pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        if pages.contains_key(&address) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let generation = self.generation();
        pages.insert(
            address,
            Page {
                bytes: Arc::new(RwLock::new(vec![0; PAGE_SIZE as usize])),
                protection,
                generation,
            },
        );
        Ok(generation)
    }

    pub fn alias(&self, source: u64, target: u64, protection: Protection) -> Result<u64, GuestFault> {
        let mut pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        if target % PAGE_SIZE != 0 || pages.contains_key(&target) {
            return Err(GuestFault {
                address: target,
                access: GuestAccess::Write,
            });
        }
        let bytes = pages
            .get(&source)
            .map(|page| Arc::clone(&page.bytes))
            .ok_or(GuestFault {
                address: source,
                access: GuestAccess::Read,
            })?;
        let generation = self.generation();
        pages.insert(
            target,
            Page {
                bytes,
                protection,
                generation,
            },
        );
        Ok(generation)
    }

    pub fn protect(&self, address: u64, protection: Protection) -> Result<(), GuestFault> {
        let mut pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        let page = pages.get_mut(&address).ok_or(GuestFault {
            address,
            access: GuestAccess::Write,
        })?;
        page.protection = protection;
        page.generation = self.generation();
        Ok(())
    }

    pub fn unmap(&self, address: u64) -> Result<(), GuestFault> {
        self.pages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&address)
            .ok_or(GuestFault {
                address,
                access: GuestAccess::Write,
            })?;
        Ok(())
    }

    #[must_use]
    pub fn generation_at(&self, address: u64) -> Option<u64> {
        self.pages
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(address / PAGE_SIZE * PAGE_SIZE))
            .map(|page| page.generation)
    }

    fn generation(&self) -> u64 {
        let mut generation = self.next_generation.lock().unwrap_or_else(|error| error.into_inner());
        *generation = generation.saturating_add(1);
        *generation
    }

    fn transfer(&self, address: u64, bytes: &mut [u8], access: GuestAccess) -> Result<usize, GuestFault> {
        let pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        let mut copied = 0;
        while copied < bytes.len() {
            let current = address.checked_add(copied as u64).ok_or(GuestFault {
                address: u64::MAX,
                access,
            })?;
            let base = current / PAGE_SIZE * PAGE_SIZE;
            let Some(page) = pages.get(&base).filter(|page| page.protection.permits(access)) else {
                return if copied == 0 {
                    Err(GuestFault {
                        address: current,
                        access,
                    })
                } else {
                    Ok(copied)
                };
            };
            let offset = (current - base) as usize;
            let count = (PAGE_SIZE as usize - offset).min(bytes.len() - copied);
            let page_bytes = page.bytes.read().unwrap_or_else(|error| error.into_inner());
            if access == GuestAccess::Read {
                bytes[copied..copied + count].copy_from_slice(&page_bytes[offset..offset + count]);
            }
            copied += count;
        }
        Ok(copied)
    }
}

impl GuestMemory for GuestPageStore {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        let mut copied = 0;
        while copied < length {
            let current = address.checked_add(copied as u64).ok_or(GuestFault {
                address: u64::MAX,
                access,
            })?;
            let base = current / PAGE_SIZE * PAGE_SIZE;
            let Some(_) = pages.get(&base).filter(|page| page.protection.permits(access)) else {
                return if copied == 0 {
                    Err(GuestFault {
                        address: current,
                        access,
                    })
                } else {
                    Ok(copied)
                };
            };
            let offset = (current - base) as usize;
            copied += (PAGE_SIZE as usize - offset).min(length - copied);
        }
        Ok(copied)
    }

    fn read(&self, address: u64, destination: &mut [u8]) -> Result<usize, GuestFault> {
        self.transfer(address, destination, GuestAccess::Read)
    }

    fn write(&self, address: u64, source: &[u8]) -> Result<usize, GuestFault> {
        let mut pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        let mut copied = 0;
        while copied < source.len() {
            let current = address.checked_add(copied as u64).ok_or(GuestFault {
                address: u64::MAX,
                access: GuestAccess::Write,
            })?;
            let base = current / PAGE_SIZE * PAGE_SIZE;
            let Some(page) = pages
                .get_mut(&base)
                .filter(|page| page.protection.permits(GuestAccess::Write))
            else {
                return if copied == 0 {
                    Err(GuestFault {
                        address: current,
                        access: GuestAccess::Write,
                    })
                } else {
                    Ok(copied)
                };
            };
            let offset = (current - base) as usize;
            let count = (PAGE_SIZE as usize - offset).min(source.len() - copied);
            page.bytes.write().unwrap_or_else(|error| error.into_inner())[offset..offset + count]
                .copy_from_slice(&source[copied..copied + count]);
            copied += count;
        }
        Ok(copied)
    }
}

impl InstructionFetch for GuestPageStore {
    fn fetch(&self, address: u64, destination: &mut [u8]) -> Result<(), FetchError> {
        let pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        for index in 0..destination.len() {
            let current = address.checked_add(index as u64).ok_or(FetchError)?;
            let base = current / PAGE_SIZE * PAGE_SIZE;
            let page = pages
                .get(&base)
                .filter(|page| page.protection.0 & Protection::EXECUTE.0 != 0)
                .ok_or(FetchError)?;
            destination[index] =
                page.bytes.read().unwrap_or_else(|error| error.into_inner())[(current - base) as usize];
        }
        Ok(())
    }
}

impl GuestOperandMemory for GuestPageStore {
    type Reservation = WriteReservation;
    type BatchReservation = Vec<WriteReservation>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        let mut value = [0; 8];
        let length = usize::from(bytes);
        let copied = GuestMemory::read(self, address, &mut value[..length]).map_err(|_| ())?;
        if copied != length {
            return Err(());
        }
        Ok(u64::from_le_bytes(value))
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        let length = usize::from(bytes);
        if self.probe(address, length, GuestAccess::Write).map_err(|_| ())? != length {
            return Err(());
        }
        Ok(WriteReservation { address, bytes })
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        let bytes = value.to_le_bytes();
        let _ =
            GuestMemory::write(self, reservation.address, &bytes[..usize::from(reservation.bytes)]).map_err(|_| ())?;
        Ok(())
    }
    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        writes
            .iter()
            .map(|(address, bytes)| self.reserve_write(*address, *bytes).map_err(|()| *address))
            .collect()
    }
    fn commit_write_batch(&mut self, reservations: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        if reservations.len() != values.len() {
            return Err(());
        }
        let mut pages = self.pages.lock().unwrap_or_else(|error| error.into_inner());
        for (reservation, value) in reservations.into_iter().zip(values) {
            let base = reservation.address / PAGE_SIZE * PAGE_SIZE;
            let page = pages.get_mut(&base).ok_or(())?;
            let offset = (reservation.address - base) as usize;
            let length = usize::from(reservation.bytes);
            page.bytes.write().unwrap_or_else(|error| error.into_inner())[offset..offset + length]
                .copy_from_slice(&value.to_le_bytes()[..length]);
        }
        Ok(())
    }
}
