use super::*;
use crate::{GraphPruneReport, PrunedGraph};

impl Images {
    /// Remove content and snapshots unreachable from named images and active leases.
    ///
    /// # Errors
    /// Returns an error when metadata cannot be read or unreachable data cannot be removed.
    pub fn gc(&self) -> Result<GcReport> {
        let _operation = crate::storage::ExclusiveLock::acquire(&self.operation_lock)?;
        let mut marked = HashSet::new();
        let mut stack: Vec<Descriptor> = self.metadata.graphs()?.into_iter().map(|graph| graph.target).collect();
        while let Some(descriptor) = stack.pop() {
            let digest: Digest = descriptor.digest().to_string().parse()?;
            if !marked.insert(digest.clone()) {
                continue;
            }
            if descriptor.is_index() {
                let doc: IndexDocument = serde_json::from_slice(&self.content.read_document(&descriptor)?)?;
                stack.extend(doc.manifests.into_iter().map(|entry| entry.descriptor));
            } else if descriptor.is_manifest() {
                let doc: ManifestDocument = serde_json::from_slice(&self.content.read_document(&descriptor)?)?;
                stack.push(doc.config);
                stack.extend(doc.layers);
            }
        }
        for lease in self.leases.list()? {
            for value in lease
                .resources()
                .filter_map(|resource| resource.strip_prefix("content:"))
            {
                marked.insert(value.parse()?);
            }
        }
        let mut report = GcReport::default();
        for digest in self.content.digests()? {
            if marked.contains(&digest) {
                report.content_kept += 1;
                continue;
            }
            let size = self.content.info(&digest)?.size;
            if self.content.remove(&digest)? {
                report.content_removed += 1;
                report.content_bytes_removed = report.content_bytes_removed.saturating_add(size);
            }
        }
        let snapshot_roots: HashSet<String> = self
            .leases
            .list()?
            .into_iter()
            .flat_map(|lease| lease.resources().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|resource| resource.strip_prefix("snapshot:").map(str::to_owned))
            .collect();
        for snapshot in self.snapshots.committed()? {
            if snapshot_roots.contains(snapshot.as_str()) {
                report.snapshots_kept += 1;
            } else if self.snapshots.remove(&snapshot)? {
                report.snapshots_removed += 1;
            }
        }
        Ok(report)
    }

    /// Atomically forget selected untagged graph records and reclaim only content from those
    /// graphs that is no longer reachable from a remaining name or lease.
    ///
    /// # Errors
    /// Returns an error for invalid graph metadata, missing content, or durable-store failures.
    pub fn prune_graphs(&self, digests: &BTreeSet<String>) -> Result<GraphPruneReport> {
        self.prune_graphs_with(digests, crate::GraphPruneScope::Dangling)
    }

    /// Atomically forget selected graphs within the requested name scope.
    ///
    /// A workspace composition applies the selection only to its workspace-owned catalog. External
    /// fallback catalogs are shared read sources and are not mutated implicitly. The returned graph
    /// list identifies only records whose workspace catalog committed their removal.
    ///
    /// Callers selecting `AllUnused` must exclude graphs referenced by live
    /// containers or another external owner before invoking this operation.
    /// Leases remain authoritative for content retention in either mode.
    ///
    /// # Errors
    /// Returns an error for invalid graph metadata, missing content, or durable-store failures.
    pub fn prune_graphs_with(
        &self,
        digests: &BTreeSet<String>,
        scope: crate::GraphPruneScope,
    ) -> Result<GraphPruneReport> {
        self.prune_local_graphs_with(digests, scope)
    }

    fn prune_local_graphs_with(
        &self,
        digests: &BTreeSet<String>,
        scope: crate::GraphPruneScope,
    ) -> Result<GraphPruneReport> {
        let mut report = GraphPruneReport::default();
        for (id, pending) in self.metadata.pending_prunes()? {
            self.remove_pending(&id, &pending.content, &mut report.gc)?;
        }

        let (generation, graphs) = self.metadata.graph_snapshot()?;
        let selected = graphs
            .iter()
            .filter(|graph| {
                digests.contains(&graph.target.digest().to_string())
                    && (graph.names.is_empty() || graph.build_cache || scope == crate::GraphPruneScope::AllUnused)
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Ok(report);
        }
        let mut candidates = BTreeMap::new();
        for graph in &selected {
            for descriptor in crate::DescriptorGraph::walk(graph.target.clone(), &self.content)? {
                candidates.insert(descriptor.digest().to_string(), descriptor.size());
            }
        }

        let mut live = HashSet::new();
        for graph in graphs.iter().filter(|graph| {
            !selected
                .iter()
                .any(|selected| selected.target.digest() == graph.target.digest())
        }) {
            live.extend(
                crate::DescriptorGraph::walk(graph.target.clone(), &self.content)?
                    .into_iter()
                    .map(|descriptor| descriptor.digest().to_string()),
            );
        }
        for lease in self.leases.list()? {
            for value in lease
                .resources()
                .filter_map(|resource| resource.strip_prefix("content:"))
            {
                live.insert(value.to_owned());
            }
        }

        candidates.retain(|digest, _| {
            if live.contains(digest) {
                report.gc.content_kept += 1;
                false
            } else {
                true
            }
        });
        let Some((id, pending)) = self.metadata.stage_prune(generation, digests, candidates, scope)? else {
            return Ok(report);
        };
        self.remove_pending(&id, &pending.content, &mut report.gc)?;
        report
            .graphs_removed
            .extend(selected.into_iter().map(|graph| PrunedGraph {
                target: graph.target.clone(),
                names: graph.names.clone(),
            }));
        Ok(report)
    }

    fn remove_pending(&self, id: &str, content: &BTreeMap<String, u64>, report: &mut GcReport) -> Result<()> {
        for (value, size) in content {
            let digest: Digest = value.parse()?;
            if self.content.remove(&digest)? {
                report.content_removed += 1;
                report.content_bytes_removed = report.content_bytes_removed.saturating_add(*size);
            }
        }
        self.metadata.finish_prune(id)
    }
}
