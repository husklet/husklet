#![allow(unused_imports, dead_code)]

/// Format a byte count compactly (B / KB / MB / GB).
pub(crate) fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b < 1024.0 {
        format!("{} B", bytes.max(0))
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_thresholds() {
        // Negative and zero clamp to "0 B".
        assert_eq!(human_size(-5), "0 B");
        assert_eq!(human_size(0), "0 B");
        // Bytes below 1 KiB print raw.
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1023), "1023 B");
        // KB: integer precision.
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(1536), "2 KB"); // 1.5 -> {:.0} rounds to 2
        // MB: one decimal.
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
        // GB: one decimal.
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
    }
}
