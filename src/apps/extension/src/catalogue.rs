//! What the extension knows about the workspace's containers.
//!
//! Containers are held here and served as row windows rather than drawn into
//! the interface directly, so the table costs a viewport however many there are.

use hl_extension::port::ContainerSummary;
use hl_extension::Request;
use hl_gui::{Cell, Row, RowRequest, RowWindow, SourceMutation, Tone, Version};

use crate::SOURCE;

/// The containers this extension is showing.
pub struct Catalogue {
    containers: Vec<ContainerSummary>,
    version: Version,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalogue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            containers: Vec::new(),
            version: Version::new(1),
        }
    }

    #[must_use]
    pub fn containers(&self) -> &[ContainerSummary] {
        &self.containers
    }

    #[must_use]
    pub const fn version(&self) -> Version {
        self.version
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.containers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// How many are in each state, for the summary line.
    #[must_use]
    pub fn running(&self) -> usize {
        self.containers
            .iter()
            .filter(|container| container.state == "running")
            .count()
    }

    /// Replaces the listing, which starts a new generation so the host discards
    /// rows describing the previous one.
    pub fn replace(&mut self, containers: Vec<ContainerSummary>) {
        self.containers = containers;
        self.version = self.version.next();
    }

    /// The call announcing the new row count.
    #[must_use]
    pub fn resize(&self) -> Vec<Request> {
        vec![Request::SourceResize {
            mutation: SourceMutation::Length {
                source: SOURCE,
                version: self.version,
                rows: self.containers.len() as u64,
            },
        }]
    }

    /// Answers one window, clipped to what actually exists.
    #[must_use]
    pub fn window(&self, request: &RowRequest) -> RowWindow {
        let start = usize::try_from(request.range.start).unwrap_or(usize::MAX);
        let end = start
            .saturating_add(request.range.count as usize)
            .min(self.containers.len());
        let rows = self
            .containers
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(offset, container)| row(request.range.start + offset as u64, container))
            .collect();
        RowWindow {
            source: SOURCE,
            version: self.version,
            request: request.id,
            range: request.range,
            rows,
        }
    }
}

/// One container as a table row. Cells stay typed, so the host decides how a
/// size or a status is presented rather than receiving a formatted string.
fn row(index: u64, container: &ContainerSummary) -> Row {
    Row::new(
        index,
        [
            Cell::text(&container.name),
            Cell::text(&container.image),
            Cell::badge(&container.state, tone(&container.state)),
            Cell::Stamp(container.created),
        ],
    )
}

fn tone(state: &str) -> Tone {
    match state {
        "running" => Tone::Positive,
        "restarting" | "paused" => Tone::Warning,
        "exited" | "dead" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::Catalogue;
    use hl_extension::port::ContainerSummary;
    use hl_gui::{Cell, RequestId, RowRange, RowRequest, Tone, Version};

    fn container(name: &str, state: &str) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-{name}"),
            generation: 0,
            name: name.into(),
            image: "alpine:3.20".into(),
            state: state.into(),
            created: 1_700_000_000,
        }
    }

    fn request(start: u64, count: u32) -> RowRequest {
        RowRequest {
            id: RequestId::new(1),
            source: crate::SOURCE,
            version: Version::new(1),
            range: RowRange::new(start, count),
            sort: None,
            filter: None,
        }
    }

    #[test]
    fn a_window_is_clipped_to_what_exists() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![container("api", "running"), container("db", "exited")]);

        let window = catalogue.window(&request(0, 128));

        assert_eq!(window.rows.len(), 2, "a window never invents rows");
        assert_eq!(window.range.count, 128, "but it still answers the range asked for");
    }

    #[test]
    fn a_window_past_the_end_is_empty_rather_than_a_failure() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![container("api", "running")]);

        assert!(catalogue.window(&request(500, 128)).rows.is_empty());
    }

    #[test]
    fn state_becomes_a_toned_badge_rather_than_a_formatted_string() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![
            container("api", "running"),
            container("db", "exited"),
            container("cache", "restarting"),
            container("job", "created"),
        ]);

        let window = catalogue.window(&request(0, 4));
        let tones: Vec<Tone> = window
            .rows
            .iter()
            .map(|row| match &row.cells[2] {
                Cell::Badge { tone, .. } => *tone,
                other => panic!("expected a badge, got {other:?}"),
            })
            .collect();

        assert_eq!(
            tones,
            vec![Tone::Positive, Tone::Danger, Tone::Warning, Tone::Neutral],
            "the host decides how a status looks; the extension only says what it means"
        );
    }

    #[test]
    fn a_new_listing_starts_a_new_generation() {
        let mut catalogue = Catalogue::new();
        let before = catalogue.version();

        catalogue.replace(vec![container("api", "running")]);

        assert!(
            catalogue.version() > before,
            "rows describing the previous listing must not survive alongside the new one"
        );
    }

    #[test]
    fn the_summary_counts_only_what_is_running() {
        let mut catalogue = Catalogue::new();
        catalogue.replace(vec![
            container("api", "running"),
            container("worker", "running"),
            container("db", "exited"),
        ]);

        assert_eq!(catalogue.running(), 2);
        assert_eq!(catalogue.len(), 3);
    }
}
