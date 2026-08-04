use hl_vfs::{GuestName, GuestPathBytes, PathError};

#[test]
fn linux_names_boundary() {
    assert_eq!(GuestPathBytes::new(b"").unwrap().as_bytes(), b"");
    assert_eq!(GuestPathBytes::new(b"/bin/\xff").unwrap().as_bytes(), b"/bin/\xff",);
    assert_eq!(GuestPathBytes::new(b"a\0b"), Err(PathError::ContainsNul));
    assert!(GuestPathBytes::new(&vec![b'x'; 4095]).is_ok());
    assert_eq!(GuestPathBytes::new(&vec![b'x'; 4096]), Err(PathError::TooLong),);
}

#[test]
fn component_bounds_bytes() {
    assert_eq!(GuestName::new(b"\xff").unwrap().as_bytes(), b"\xff");
    assert!(GuestName::new(&vec![b'x'; 255]).is_ok());
    assert_eq!(GuestName::new(&vec![b'x'; 256]), Err(PathError::TooLong),);
    for invalid in [b"".as_slice(), b".", b"..", b"a/b", b"a\0b"] {
        assert!(GuestName::new(invalid).is_err());
    }
}
