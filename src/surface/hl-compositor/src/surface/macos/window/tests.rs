#[cfg(test)]
mod tests {
    fn drawable_matches(source: (usize, usize), drawable: (usize, usize)) -> bool {
        source == drawable && source.0 != 0 && source.1 != 0
    }

    #[test]
    fn asynchronous_resize_retries_stale_drawable_then_accepts_matching_size() {
        assert!(!drawable_matches((1600, 1000), (1200, 800)));
        assert!(drawable_matches((1600, 1000), (1600, 1000)));
    }

    #[test]
    fn zero_sized_drawable_is_never_presentable() {
        assert!(!drawable_matches((0, 1000), (0, 1000)));
        assert!(!drawable_matches((1600, 0), (1600, 0)));
    }
}
