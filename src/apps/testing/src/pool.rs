use std::future::Future;
use tokio::task::{JoinError, JoinSet};

/// A bounded work frontier: at most `jobs` tasks run at once, and the rest wait in `pending`.
pub(crate) struct Pool<T, R> {
    pending: std::vec::IntoIter<T>,
    running: JoinSet<R>,
    jobs: usize,
}

impl<T: Send + 'static, R: Send + 'static> Pool<T, R> {
    pub fn new(work: Vec<T>, jobs: usize) -> Self {
        Self {
            pending: work.into_iter(),
            running: JoinSet::new(),
            jobs,
        }
    }

    pub async fn next<F, Fut>(&mut self, launch: F) -> Result<Option<R>, JoinError>
    where
        F: Clone + Fn(T) -> Fut,
        Fut: Future<Output = R> + Send + 'static,
    {
        while self.running.len() < self.jobs {
            let Some(work) = self.pending.next() else { break };
            self.running.spawn(launch.clone()(work));
        }
        self.running.join_next().await.transpose()
    }

    /// Cancels the active frontier and waits until every task destructor has
    /// finished. Row destructors own external worker cleanup, so dropping the
    /// JoinSet without this barrier can return while worker groups still live.
    pub async fn shutdown(&mut self) {
        self.running.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::Pool;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};
    use tokio::{sync::Semaphore, task::JoinSet};

    #[tokio::test]
    async fn large_queue_only_spawns_the_bounded_frontier() {
        let started = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let launch = {
            let started = Arc::clone(&started);
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            move |ordinal| {
                let started = Arc::clone(&started);
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    if ordinal < 4 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    } else {
                        tokio::task::yield_now().await;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    ordinal
                }
            }
        };
        let mut pool = Pool::new((0..10_000).collect(), 4);
        let first = pool.next(launch.clone()).await.unwrap().unwrap();
        assert!(first < 4);
        assert_eq!(started.load(Ordering::SeqCst), 4);
        let mut completed = 1;
        while pool.next(launch.clone()).await.unwrap().is_some() {
            completed += 1;
        }
        assert_eq!(completed, 10_000);
        assert_eq!(maximum.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn bounded_rows_overlap_without_serializing() {
        let launch = |ordinal| async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            ordinal
        };
        let started = Instant::now();
        let mut pool = Pool::new((0..6).collect(), 2);
        let mut order = Vec::new();
        while let Some(value) = pool.next(launch).await.unwrap() {
            order.push(value);
        }
        order.sort_unstable();
        assert_eq!(order, (0..6).collect::<Vec<_>>());
        assert!(started.elapsed() >= Duration::from_millis(55));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn shutdown_waits_for_cancelled_task_destructors() {
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                std::thread::sleep(Duration::from_millis(20));
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let mut pool = Pool::new((0..4).collect(), 4);
        let observed = Arc::clone(&dropped);
        let launch = move |_| {
            let guard = Guard(Arc::clone(&observed));
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            }
        };
        // Fill the frontier, then cancel it through the same barrier used by a
        // sweep abort. The timeout only proves every task reached its pending point.
        let _ = tokio::time::timeout(Duration::from_millis(30), pool.next(launch)).await;
        pool.shutdown().await;
        assert_eq!(dropped.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    #[ignore = "manual scheduler wall comparison"]
    async fn scheduler_wall_comparison() {
        const ROWS: usize = 20_000;
        let eager_started = Instant::now();
        let permits = Arc::new(Semaphore::new(1));
        let mut eager = JoinSet::new();
        for ordinal in 0..ROWS {
            let permits = Arc::clone(&permits);
            eager.spawn(async move {
                let _permit = permits.acquire_owned().await.unwrap();
                ordinal
            });
        }
        let mut eager_count = 0;
        while eager.join_next().await.is_some() {
            eager_count += 1;
        }
        let eager_elapsed = eager_started.elapsed();

        let bounded_started = Instant::now();
        let mut bounded = Pool::new((0..ROWS).collect(), 1);
        let mut bounded_count = 0;
        while bounded.next(|ordinal| async move { ordinal }).await.unwrap().is_some() {
            bounded_count += 1;
        }
        let bounded_elapsed = bounded_started.elapsed();
        assert_eq!(eager_count, ROWS);
        assert_eq!(bounded_count, ROWS);
        eprintln!("runtime scheduler rows={ROWS} eager={eager_elapsed:?} bounded={bounded_elapsed:?}");
    }
}
