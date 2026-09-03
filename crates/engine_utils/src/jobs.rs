//! [`JobSystem`] — a thin, safe wrapper over a `rayon` thread pool for
//! spreading CPU work (culling, animation, particle simulation, scattering)
//! across cores.
//!
//! Owning the pool (rather than calling `rayon`'s global free functions)
//! means a known thread count and that nested `rayon` / `par_*` calls
//! inside [`JobSystem::install`] route to *this* pool. `JobSystem` is
//! `Arc`-backed and [`Clone`], so subsystems share one pool cheaply.
//!
//! Deliberately minimal: `join`, a `scope` for a dynamic task count, and
//! order-preserving `par_map` / `par_for_each`. A persistent job graph,
//! job handles, priorities, and per-job profiling spans are future work.
//!
//! ```
//! use engine_utils::JobSystem;
//!
//! let jobs = JobSystem::new()?;
//! let squares = jobs.par_map(&[1, 2, 3, 4], |n| n * n);
//! assert_eq!(squares, vec![1, 4, 9, 16]);
//! # Ok::<(), engine_utils::JobError>(())
//! ```

use std::sync::Arc;

/// Errors from building a [`JobSystem`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JobError {
    /// [`JobSystem::with_threads`] was given `0`.
    #[error("job system thread count must be at least 1")]
    InvalidThreadCount,
    /// The underlying `rayon` thread pool failed to build.
    #[error("failed to build job system thread pool: {0}")]
    PoolBuild(String),
}

/// A shared, work-stealing thread pool for parallel CPU work.
///
/// Cheap to [`Clone`] (an `Arc` bump) — build one at startup and hand
/// `&JobSystem` (or a clone) to whatever needs parallelism.
#[derive(Clone)]
pub struct JobSystem {
    pool: Arc<rayon::ThreadPool>,
}

impl JobSystem {
    /// A pool with `rayon`'s default thread count (usually one worker per
    /// logical core).
    ///
    /// # Errors
    ///
    /// [`JobError::PoolBuild`] if the OS refuses to start the worker
    /// threads.
    pub fn new() -> Result<Self, JobError> {
        Self::build(rayon::ThreadPoolBuilder::new())
    }

    /// A pool with exactly `threads` worker threads.
    ///
    /// # Errors
    ///
    /// [`JobError::InvalidThreadCount`] if `threads` is `0`;
    /// [`JobError::PoolBuild`] if the pool can't be created.
    pub fn with_threads(threads: usize) -> Result<Self, JobError> {
        if threads == 0 {
            return Err(JobError::InvalidThreadCount);
        }
        Self::build(rayon::ThreadPoolBuilder::new().num_threads(threads))
    }

    fn build(builder: rayon::ThreadPoolBuilder) -> Result<Self, JobError> {
        let pool = builder
            .build()
            .map_err(|err| JobError::PoolBuild(err.to_string()))?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    /// Number of worker threads in the pool.
    pub fn thread_count(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// Runs `f` on the pool and returns its value. Any `rayon` /
    /// [`JobSystem`] parallel work started inside `f` uses this pool
    /// rather than `rayon`'s global one.
    pub fn install<R, F>(&self, f: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        self.pool.install(f)
    }

    /// Runs `a` and `b`, potentially in parallel, and returns both
    /// results. Result values are deterministic; side-effect ordering
    /// between `a` and `b` is not.
    pub fn join<A, B, RA, RB>(&self, a: A, b: B) -> (RA, RB)
    where
        A: FnOnce() -> RA + Send,
        B: FnOnce() -> RB + Send,
        RA: Send,
        RB: Send,
    {
        self.pool.join(a, b)
    }

    /// Opens a scope: `f` may spawn a dynamic number of tasks via
    /// [`Scope::run`], and every one of them has finished by the time
    /// `scope` returns. `f`'s return value is passed back out.
    pub fn scope<'scope, R, F>(&self, f: F) -> R
    where
        F: FnOnce(&Scope<'scope, '_>) -> R + Send,
        R: Send,
    {
        self.pool
            .scope(|rayon_scope| f(&Scope { inner: rayon_scope }))
    }

    /// Applies `f` to every element of `items` in parallel and collects
    /// the results **in the original order**.
    pub fn par_map<T, U, F>(&self, items: &[T], f: F) -> Vec<U>
    where
        T: Sync,
        U: Send,
        F: Fn(&T) -> U + Sync + Send,
    {
        use rayon::prelude::*;
        self.pool.install(|| items.par_iter().map(&f).collect())
    }

    /// Applies `f` to every element of `items` in parallel, mutating them
    /// in place.
    pub fn par_for_each<T, F>(&self, items: &mut [T], f: F)
    where
        T: Send,
        F: Fn(&mut T) + Sync + Send,
    {
        use rayon::prelude::*;
        self.pool.install(|| items.par_iter_mut().for_each(&f));
    }
}

/// A task-spawning handle for [`JobSystem::scope`]. Tasks spawned through
/// it all complete before the `scope` call returns.
///
/// `'scope` is the lifetime data borrowed by spawned tasks must outlive;
/// `'a` is the borrow of the underlying `rayon` scope.
pub struct Scope<'scope, 'a> {
    inner: &'a rayon::Scope<'scope>,
}

impl<'scope> Scope<'scope, '_> {
    /// Spawns `task` into the scope. It runs on some pool thread and is
    /// guaranteed to have finished before the enclosing
    /// [`JobSystem::scope`] returns.
    pub fn run(&self, task: impl FnOnce() + Send + 'scope) {
        self.inner.spawn(move |_| task());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn new_builds_a_pool_with_at_least_one_thread() {
        let jobs = JobSystem::new().expect("default pool builds");
        assert!(jobs.thread_count() >= 1);
    }

    #[test]
    fn with_threads_sets_the_worker_count() {
        assert_eq!(JobSystem::with_threads(4).unwrap().thread_count(), 4);
        assert_eq!(JobSystem::with_threads(1).unwrap().thread_count(), 1);
    }

    #[test]
    fn with_threads_zero_is_rejected() {
        assert!(matches!(
            JobSystem::with_threads(0),
            Err(JobError::InvalidThreadCount)
        ));
    }

    #[test]
    fn clone_shares_the_same_pool() {
        let jobs = JobSystem::with_threads(3).unwrap();
        let clone = jobs.clone();
        assert_eq!(jobs.thread_count(), clone.thread_count());
    }

    #[test]
    fn join_runs_both_and_returns_both_results() {
        let jobs = JobSystem::new().unwrap();
        let (a, b) = jobs.join(|| 2 + 2, || "ok");
        assert_eq!(a, 4);
        assert_eq!(b, "ok");
    }

    #[test]
    fn par_map_preserves_order() {
        let jobs = JobSystem::new().unwrap();
        assert_eq!(
            jobs.par_map(&[1, 2, 3, 4, 5], |n| n * n),
            vec![1, 4, 9, 16, 25]
        );
        assert!(jobs.par_map::<i32, i32, _>(&[], |n| *n).is_empty());
    }

    #[test]
    fn par_for_each_mutates_in_place() {
        let jobs = JobSystem::new().unwrap();
        let mut values = [1, 2, 3, 4];
        jobs.par_for_each(&mut values, |n| *n *= 10);
        assert_eq!(values, [10, 20, 30, 40]);
    }

    #[test]
    fn scope_waits_for_every_spawned_task() {
        let jobs = JobSystem::with_threads(4).unwrap();
        let counter = AtomicUsize::new(0);
        const TASKS: usize = 200;

        jobs.scope(|scope| {
            for _ in 0..TASKS {
                scope.run(|| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });
            }
        });

        // Every task completed before `scope` returned.
        assert_eq!(counter.load(Ordering::Relaxed), TASKS);
    }

    #[test]
    fn scope_returns_the_closures_value() {
        let jobs = JobSystem::new().unwrap();
        let value = jobs.scope(|_scope| 7 * 6);
        assert_eq!(value, 42);
    }

    #[test]
    fn install_routes_nested_rayon_to_this_pool() {
        let jobs = JobSystem::with_threads(2).unwrap();
        let seen = jobs.install(rayon::current_num_threads);
        assert_eq!(seen, 2);
    }

    #[test]
    fn par_map_matches_a_serial_sum_over_a_large_slice() {
        let jobs = JobSystem::new().unwrap();
        let data: Vec<u64> = (0..10_000).collect();
        let parallel: u64 = jobs.par_map(&data, |n| n * 2).iter().sum();
        let serial: u64 = data.iter().map(|n| n * 2).sum();
        assert_eq!(parallel, serial);
    }

    #[test]
    fn job_error_displays_both_variants() {
        assert!(
            JobError::InvalidThreadCount
                .to_string()
                .contains("at least 1")
        );
        assert!(
            JobError::PoolBuild("boom".into())
                .to_string()
                .contains("boom")
        );
    }
}
