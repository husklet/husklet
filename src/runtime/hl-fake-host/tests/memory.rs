use hl_execution::InstructionFetch;
use hl_fake_host::{GuestPageStore, PAGE_SIZE, PageProtection};
use hl_linux::{GuestAccess, GuestMemory};

#[test]
fn boundary_reports_exact() {
    let memory = GuestPageStore::default();
    memory.map(0, PageProtection::READ_WRITE).unwrap();
    assert_eq!(memory.write(PAGE_SIZE - 2, &[1, 2, 3, 4]), Ok(2));
    assert_eq!(
        memory.probe(PAGE_SIZE, 1, GuestAccess::Read).unwrap_err().address,
        PAGE_SIZE
    );
    memory.map(PAGE_SIZE, PageProtection::READ_WRITE).unwrap();
    assert_eq!(memory.write(PAGE_SIZE - 2, &[1, 2, 3, 4]), Ok(4));
    let mut bytes = [0; 4];
    assert_eq!(memory.read(PAGE_SIZE - 2, &mut bytes), Ok(4));
    assert_eq!(bytes, [1, 2, 3, 4]);
}

#[test]
fn aliases_share_bytes() {
    let memory = GuestPageStore::default();
    let initial = memory.map(0, PageProtection::READ_WRITE).unwrap();
    let alias = memory.alias(0, PAGE_SIZE * 2, PageProtection::READ).unwrap();
    memory.write(7, &[42]).unwrap();
    let mut byte = [0];
    memory.read(PAGE_SIZE * 2 + 7, &mut byte).unwrap();
    assert_eq!(byte, [42]);
    assert_ne!(initial, alias);
    assert!(memory.write(PAGE_SIZE * 2, &[1]).is_err());
}

#[test]
fn instruction_fetch_requires() {
    let memory = GuestPageStore::default();
    memory.map(0, PageProtection::READ_WRITE).unwrap();
    memory.write(0, &[0xaa]).unwrap();
    let before = memory.generation_at(0).unwrap();
    assert!(memory.fetch(0, &mut [0]).is_err());
    memory
        .protect(0, PageProtection::READ.union(PageProtection::EXECUTE))
        .unwrap();
    let mut byte = [0];
    memory.fetch(0, &mut byte).unwrap();
    assert_eq!(byte, [0xaa]);
    assert!(memory.generation_at(0).unwrap() > before);
}
