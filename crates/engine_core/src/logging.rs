//! Structured logging setup.
//!
//! The engine emits [`tracing`] events internally (startup, shutdown,
//! errors, warnings, ...); it never installs a subscriber implicitly, since
//! a host application may want its own. [`init_default`] is an opt-in
//! convenience for apps (including the example `game` binary) that just
//! want sensible console output.

use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::error::EngineError;

/// The verbosity filter both [`init_default`] and
/// [`init_default_with_layer`] use: the `RUST_LOG` environment variable
/// (standard `tracing-subscriber` `EnvFilter` syntax, e.g.
/// `RUST_LOG=engine_core=debug`), defaulting to `info` when unset.
fn env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

/// Installs a default `tracing` subscriber that logs to stderr.
///
/// Verbosity is controlled by the `RUST_LOG` environment variable
/// (standard `tracing-subscriber` `EnvFilter` syntax, e.g.
/// `RUST_LOG=engine_core=debug`); defaults to `info` when unset.
///
/// # Errors
///
/// Returns [`EngineError::LoggingInit`] if a global subscriber is already
/// installed (e.g. this was called twice, or the host app installed its
/// own first).
///
/// # Example
///
/// ```no_run
/// // `no_run`: installs process-global state, unsafe to run repeatedly
/// // inside the merged doctest process.
/// engine_core::logging::init_default()?;
/// # Ok::<(), engine_core::EngineError>(())
/// ```
pub fn init_default() -> Result<(), EngineError> {
    tracing_subscriber::fmt()
        .with_env_filter(env_filter())
        .try_init()
        .map_err(|err| EngineError::LoggingInit(err.to_string()))
}

/// Like [`init_default`], but also feeds every event to `layer` at the
/// same verbosity — e.g. the editor's console panel, which needs its own
/// copy of each log line alongside the normal stderr output.
///
/// # Errors
///
/// Same as [`init_default`].
pub fn init_default_with_layer<L>(layer: L) -> Result<(), EngineError>
where
    L: Layer<tracing_subscriber::Registry> + Send + Sync + 'static,
{
    // `Layer::and_then` combines two sibling layers (both `Layer<S>` for
    // the same `S`) into one, rather than each nesting the subscriber
    // type like chained `.with()` calls would — the latter needs `layer`
    // to implement `Layer` for that ever-growing nested type, which a
    // caller-supplied `L: Layer<Registry>` has no way to prove.
    let fmt_layer = tracing_subscriber::fmt::layer().with_filter(env_filter());
    let layer = layer.with_filter(env_filter());

    tracing_subscriber::registry()
        .with(fmt_layer.and_then(layer))
        .try_init()
        .map_err(|err| EngineError::LoggingInit(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_init_fails_once_a_subscriber_is_installed() {
        // First call may succeed or already be Err if an earlier test in
        // this binary installed a subscriber first (test order is
        // unspecified) - either way, calling it twice in a row must fail
        // the second time.
        let _ = init_default();
        let err = init_default().unwrap_err();
        assert!(matches!(err, EngineError::LoggingInit(_)));
    }

    #[test]
    fn init_default_with_layer_also_fails_once_a_subscriber_is_installed() {
        // Same cross-test nondeterminism as above: some subscriber (from
        // this test or another) is already global by the second call.
        struct NoopLayer;
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for NoopLayer {}

        let _ = init_default_with_layer(NoopLayer);
        let err = init_default_with_layer(NoopLayer).unwrap_err();
        assert!(matches!(err, EngineError::LoggingInit(_)));
    }
}
