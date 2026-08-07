//! Address-space and memory-accounting projections used by procfs.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryView {
    pub page_bytes: u64,
    pub total_pages: u64,
    pub resident_pages: u64,
    pub shared_pages: u64,
    pub text_pages: u64,
    pub data_pages: u64,
}

/// One immutable, generation-qualified view of a guest address space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddressSpaceView {
    pub generation: u64,
    pub page_bytes: u64,
    pub regions: Vec<MemoryRegionView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryRegionView {
    pub start: u64,
    pub end: u64,
    pub protection: u8,
    pub shared: bool,
    pub backing_offset: u64,
    pub device: u64,
    pub inode: u64,
    pub path: Option<Vec<u8>>,
    pub label: Option<MemoryRegionLabel>,
    pub resident_pages: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryRegionLabel {
    Heap,
    Stack,
    StackGuard,
}

impl AddressSpaceView {
    #[must_use]
    pub fn new(generation: u64, page_bytes: u64, regions: Vec<MemoryRegionView>) -> Option<Self> {
        if generation == 0 || !page_bytes.is_power_of_two() {
            return None;
        }
        let valid = regions.iter().enumerate().all(|(index, region)| {
            let pages = region
                .end
                .checked_sub(region.start)
                .map(|bytes| bytes.div_ceil(page_bytes));
            region.start < region.end
                && region.protection & !7 == 0
                && pages.is_some_and(|pages| region.resident_pages <= pages)
                && region.path.as_ref().is_none_or(|path| !path.contains(&0))
                && index
                    .checked_sub(1)
                    .is_none_or(|previous| regions[previous].end <= region.start)
        });
        valid.then_some(Self {
            generation,
            page_bytes,
            regions,
        })
    }

    #[must_use]
    pub fn maps(&self, smaps: bool) -> Vec<u8> {
        let mut output = Vec::new();
        for region in &self.regions {
            let read = if region.protection & 1 != 0 { 'r' } else { '-' };
            let write = if region.protection & 2 != 0 { 'w' } else { '-' };
            let execute = if region.protection & 4 != 0 { 'x' } else { '-' };
            let sharing = if region.shared { 's' } else { 'p' };
            let major = (region.device >> 8) & 0xff;
            let minor = region.device & 0xff;
            output.extend_from_slice(
                format!(
                    "{:08x}-{:08x} {read}{write}{execute}{sharing} {:08x} {major:02x}:{minor:02x} {}",
                    region.start, region.end, region.backing_offset, region.inode,
                )
                .as_bytes(),
            );
            if let Some(path) = &region.path {
                output.push(b' ');
                output.extend_from_slice(path);
            } else if let Some(label) = region.label {
                output.extend_from_slice(match label {
                    MemoryRegionLabel::Heap => b" [heap]".as_slice(),
                    MemoryRegionLabel::Stack => b" [stack]",
                    MemoryRegionLabel::StackGuard => b"",
                });
            }
            output.push(b'\n');
            if smaps {
                let pages = region.end.saturating_sub(region.start).div_ceil(self.page_bytes);
                let size = pages.saturating_mul(self.page_bytes) / 1024;
                let resident = region.resident_pages.saturating_mul(self.page_bytes) / 1024;
                output.extend_from_slice(
                    format!(
                        "Size:{size:19} kB\nKernelPageSize:{:9} kB\nMMUPageSize:{:12} kB\n\
                     Rss:{resident:20} kB\nPss:{resident:20} kB\nShared_Clean:{:11} kB\n\
                     Shared_Dirty:{:11} kB\nPrivate_Clean:{:10} kB\nPrivate_Dirty:{:10} kB\n\
                     Referenced:{resident:13} kB\nAnonymous:{:14} kB\nSwap:{:19} kB\nLocked:{:17} kB\n\
                     VmFlags:{}{}{} mr mw me ac \n",
                        self.page_bytes / 1024,
                        self.page_bytes / 1024,
                        0,
                        if region.shared { resident } else { 0 },
                        if region.inode != 0 { resident } else { 0 },
                        if !region.shared && region.inode == 0 {
                            resident
                        } else {
                            0
                        },
                        if region.inode == 0 { resident } else { 0 },
                        0,
                        0,
                        if read == 'r' { " rd" } else { "" },
                        if write == 'w' { " wr" } else { "" },
                        if execute == 'x' { " ex" } else { "" },
                    )
                    .as_bytes(),
                );
            }
        }
        output
    }

    fn numa_file_field(path: &[u8]) -> Vec<u8> {
        let mut field = b" file=".to_vec();
        field.extend(path.iter().flat_map(|byte| {
            if *byte == b' ' {
                br"\040".as_slice()
            } else {
                std::slice::from_ref(byte)
            }
        }));
        field
    }

    #[must_use]
    pub fn numa(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for region in &self.regions {
            output.extend_from_slice(format!("{:08x} default", region.start).as_bytes());
            match region.label {
                Some(MemoryRegionLabel::Heap) => output.extend_from_slice(b" heap"),
                Some(MemoryRegionLabel::Stack) => output.extend_from_slice(b" stack"),
                _ if region.inode != 0 => {
                    output.extend(region.path.iter().flat_map(|path| Self::numa_file_field(path)));
                }
                _ => {}
            }
            let pages = region.end.saturating_sub(region.start) / self.page_bytes;
            if region.protection != 0 && pages != 0 {
                let residency = if region.inode == 0 {
                    format!(" anon={pages} dirty={pages}")
                } else {
                    format!(" mapped={pages}")
                };
                output.extend_from_slice(residency.as_bytes());
                output.extend_from_slice(format!(" active=0 N0={pages} kernelpagesize_kB=4").as_bytes());
            }
            output.push(b'\n');
        }
        output
    }

    #[must_use]
    pub fn rollup(&self) -> Vec<u8> {
        let low = self.regions.first().map_or(0, |region| region.start);
        let high = self.regions.last().map_or(0, |region| region.end);
        let mut rss = 0_u64;
        let mut clean = 0_u64;
        let mut dirty = 0_u64;
        for region in &self.regions {
            if region.protection == 0 {
                continue;
            }
            let bytes = region.resident_pages.saturating_mul(self.page_bytes) / 1024;
            rss = rss.saturating_add(bytes);
            if region.inode == 0 {
                dirty = dirty.saturating_add(bytes);
            } else {
                clean = clean.saturating_add(bytes);
            }
        }
        format!(
            "{low:08x}-{high:08x} ---p 00000000 00:00 0 [rollup]\n\
             Rss:{rss:20} kB\nPss:{rss:20} kB\nPss_Dirty:{dirty:14} kB\n\
             Pss_Anon:{dirty:15} kB\nPss_File:{clean:15} kB\nPss_Shmem:{:14} kB\n\
             Shared_Clean:{:11} kB\nShared_Dirty:{dirty:11} kB\nPrivate_Clean:{clean:10} kB\n\
             Private_Dirty:{:10} kB\nReferenced:{rss:13} kB\nAnonymous:{dirty:14} kB\n\
             Swap:{:19} kB\nLocked:{:17} kB\n",
            0, 0, 0, 0, 0,
        )
        .into_bytes()
    }

    #[must_use]
    pub fn memory(&self) -> MemoryView {
        let mut view = MemoryView {
            page_bytes: self.page_bytes,
            total_pages: 0,
            resident_pages: 0,
            shared_pages: 0,
            text_pages: 0,
            data_pages: 0,
        };
        for region in &self.regions {
            let pages = region.end.saturating_sub(region.start).div_ceil(self.page_bytes);
            view.total_pages = view.total_pages.saturating_add(pages);
            view.resident_pages = view.resident_pages.saturating_add(region.resident_pages.min(pages));
            if region.shared {
                view.shared_pages = view.shared_pages.saturating_add(region.resident_pages.min(pages));
            }
            if region.protection & 4 != 0 {
                view.text_pages = view.text_pages.saturating_add(pages);
            } else if region.protection & 2 != 0 {
                view.data_pages = view.data_pages.saturating_add(pages);
            }
        }
        view
    }
}

impl MemoryView {
    pub(in crate::procfs) fn statm(self) -> Vec<u8> {
        format!(
            "{} {} {} {} 0 {} 0\n",
            self.total_pages, self.resident_pages, self.shared_pages, self.text_pages, self.data_pages
        )
        .into_bytes()
    }
}
