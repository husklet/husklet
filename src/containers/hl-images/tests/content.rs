use std::io::Read;

use hl_images::{
    Descriptor, Digest, Error,
    content::{FsStore, Store},
};

fn descriptor(bytes: &[u8]) -> Descriptor {
    serde_json::from_value(serde_json::json!({
        "mediaType": "application/vnd.oci.image.layer.v1.tar",
        "digest": Digest::sha256(bytes).to_string(),
        "size": bytes.len()
    }))
    .unwrap()
}

#[test]
fn staged_content_is_invisible_until_verified_commit() {
    let temp = tempfile::tempdir().unwrap();
    let store = FsStore::open(temp.path()).unwrap();
    let bytes = b"immutable content";
    let expected = descriptor(bytes);
    let digest: Digest = expected.digest().to_string().parse().unwrap();
    let mut ingest = store.ingest("pull-1").unwrap();
    ingest.write(&bytes[..5]).unwrap();
    assert!(!store.contains(&digest).unwrap());
    ingest.write(&bytes[5..]).unwrap();
    ingest.commit(&expected).unwrap();
    assert!(store.contains(&digest).unwrap());
    let mut reader = store.reader(&expected).unwrap();
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, bytes);
}

#[test]
fn mismatch_and_abort_never_publish_content() {
    let temp = tempfile::tempdir().unwrap();
    let store = FsStore::open(temp.path()).unwrap();
    let expected = descriptor(b"right");
    let digest: Digest = expected.digest().to_string().parse().unwrap();
    let mut ingest = store.ingest("bad").unwrap();
    ingest.write(b"wrong").unwrap();
    assert!(matches!(ingest.commit(&expected), Err(Error::DigestMismatch { .. })));
    assert!(!store.contains(&digest).unwrap());

    let mut ingest = store.ingest("aborted").unwrap();
    ingest.write(b"right").unwrap();
    ingest.abort().unwrap();
    assert!(!store.contains(&digest).unwrap());
    assert_eq!(std::fs::read_dir(temp.path().join("ingest")).unwrap().count(), 0);
}
