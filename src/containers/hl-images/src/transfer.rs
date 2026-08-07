use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{StreamExt, stream};

use crate::{
    Descriptor, DescriptorKind as _, Digest, Error, Reference, Result,
    content::{FsStore, Store},
    remote::{BlobStream, Source},
};

/// Supplies direct successors of an OCI descriptor (index, manifest, or artifact manifest).
pub trait Successors {
    /// # Errors
    /// Returns an error when descriptor successors cannot be decoded or loaded.
    fn successors(&self, descriptor: &Descriptor) -> Result<Vec<Descriptor>>;
}

impl Successors for FsStore {
    fn successors(&self, descriptor: &Descriptor) -> Result<Vec<Descriptor>> {
        const MAX_DOCUMENT: u64 = 16 * 1024 * 1024;
        if !descriptor.is_document() {
            return Ok(Vec::new());
        }
        if descriptor.size() > MAX_DOCUMENT {
            return Err(Error::MalformedOci("descriptor document exceeds 16 MiB".into()));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(descriptor.size()).unwrap_or(0));
        Store::reader(self, descriptor)?.read_to_end(&mut bytes)?;
        DescriptorGraph::verify(&bytes, descriptor)?;
        DescriptorGraph::successors(descriptor, &bytes)
    }
}

#[async_trait]
pub trait Target: Send + Sync {
    async fn contains(&self, descriptor: &Descriptor) -> Result<bool>;
    async fn push(&self, descriptor: &Descriptor, content: BlobStream) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyReport {
    pub copied: u64,
    pub skipped: u64,
    pub bytes: u64,
}

/// Copy a complete OCI descriptor graph without unpacking its filesystem layers.
///
/// # Errors
/// Returns an error for source/target failures, corrupt streams, or malformed descriptor documents.
pub async fn copy_graph(
    source: &(impl Source + ?Sized),
    target: &impl Target,
    reference: &Reference,
    root: Descriptor,
) -> Result<CopyReport> {
    let mut report = CopyReport::default();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(descriptor) = queue.pop_front() {
        if !seen.insert(descriptor.digest().to_string()) {
            continue;
        }
        if target.contains(&descriptor).await? {
            report.skipped += 1;
            continue;
        }
        let stream = source.fetch(reference, &descriptor).await?;
        if descriptor.is_document() {
            let bytes = DescriptorGraph::collect(stream, &descriptor).await?;
            queue.extend(DescriptorGraph::successors(&descriptor, &bytes)?);
            report.bytes += bytes.len() as u64;
            target.push(&descriptor, DescriptorGraph::collected(bytes)).await?;
        } else {
            report.bytes += descriptor.size();
            target.push(&descriptor, stream).await?;
        }
        report.copied += 1;
    }
    Ok(report)
}

/// Deterministic depth-first descriptor graph traversal with cycle and duplicate suppression.
pub struct DescriptorGraph;
impl DescriptorGraph {
    fn collected(bytes: Bytes) -> BlobStream {
        Box::pin(stream::once(async move { Ok(bytes) }))
    }

    async fn collect(mut stream: BlobStream, descriptor: &Descriptor) -> Result<Bytes> {
        const MAX_DOCUMENT: u64 = 16 * 1024 * 1024;
        if descriptor.size() > MAX_DOCUMENT {
            return Err(Error::MalformedOci("descriptor document exceeds 16 MiB".into()));
        }
        let expected = usize::try_from(descriptor.size())
            .map_err(|_| Error::MalformedOci("descriptor document exceeds host address space".into()))?;
        let mut data = Vec::with_capacity(expected);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if data.len().saturating_add(chunk.len()) > expected {
                return Err(Error::SizeMismatch {
                    expected: descriptor.size(),
                    actual: (data.len() + chunk.len()) as u64,
                });
            }
            data.extend_from_slice(&chunk);
        }
        Self::verify(&data, descriptor)?;
        Ok(data.into())
    }

    pub(crate) fn verify(bytes: &[u8], descriptor: &Descriptor) -> Result<()> {
        if bytes.len() as u64 != descriptor.size() {
            return Err(Error::SizeMismatch {
                expected: descriptor.size(),
                actual: bytes.len() as u64,
            });
        }
        let expected: Digest = descriptor.digest().to_string().parse()?;
        let actual = Digest::sha256(bytes);
        if expected != actual {
            return Err(Error::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    }

    fn successors(descriptor: &Descriptor, bytes: &[u8]) -> Result<Vec<Descriptor>> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| Error::MalformedOci(error.to_string()))?;
        let media = descriptor.media_type().to_string();
        let mut result = Vec::new();
        if media.contains("index") || media.contains("manifest.list") {
            result.extend(Self::descriptors(&value, "manifests")?);
        } else if media.contains("artifact.manifest") {
            result.extend(Self::descriptors(&value, "blobs")?);
            if let Some(subject) = value.get("subject") {
                result.push(
                    serde_json::from_value(subject.clone()).map_err(|error| Error::MalformedOci(error.to_string()))?,
                );
            }
        } else {
            let config = value
                .get("config")
                .ok_or_else(|| Error::MalformedOci("manifest has no config".into()))?;
            result
                .push(serde_json::from_value(config.clone()).map_err(|error| Error::MalformedOci(error.to_string()))?);
            result.extend(Self::descriptors(&value, "layers")?);
        }
        Ok(result)
    }

    fn descriptors(value: &serde_json::Value, field: &str) -> Result<Vec<Descriptor>> {
        let entries = value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| Error::MalformedOci(format!("document has no {field}")))?;
        entries
            .iter()
            .cloned()
            .map(|entry| serde_json::from_value(entry).map_err(|error| Error::MalformedOci(error.to_string())))
            .collect()
    }

    /// # Errors
    /// Returns the first error reported by the successor source.
    pub fn walk(root: Descriptor, source: &impl Successors) -> Result<Vec<Descriptor>> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![root];
        while let Some(descriptor) = stack.pop() {
            if !seen.insert(descriptor.digest().to_string()) {
                continue;
            }
            let mut successors = source.successors(&descriptor)?;
            successors.reverse();
            stack.extend(successors);
            result.push(descriptor);
        }
        Ok(result)
    }

    /// # Errors
    /// This in-memory source is infallible; the result preserves the generic traversal surface.
    pub fn from_edges(root: Descriptor, edges: HashMap<String, Vec<Descriptor>>) -> Result<Vec<Descriptor>> {
        struct Edges(HashMap<String, Vec<Descriptor>>);
        impl Successors for Edges {
            fn successors(&self, descriptor: &Descriptor) -> Result<Vec<Descriptor>> {
                Ok(self
                    .0
                    .get(&descriptor.digest().to_string())
                    .cloned()
                    .unwrap_or_default())
            }
        }
        Self::walk(root, &Edges(edges))
    }
}
