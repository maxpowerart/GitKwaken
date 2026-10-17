//! Optional Tracy profiling.
//!
//! The expensive paths (status, history walk/layout, refs, diffs, row
//! metadata, search) carry `tracing` spans. Without a subscriber each span
//! costs a relaxed atomic load, so they stay in release builds.
//!
//! Build with the `profile` feature (`cargo build --release --features
//! gk-core/profile`, or a `profile = ["gk-core/profile"]` feature in the app)
//! and call [`init`] first thing in `main`; then connect the Tracy profiler
//! v0.14 (the version `tracing-tracy` 0.12 / `tracy-client-sys` 0.30 bundle).

/// Installs the Tracy subscriber. Returns false when the `profile` feature is
/// off (a no-op) or a global subscriber was already set.
pub fn init() -> bool {
    #[cfg(feature = "profile")]
    {
        use tracing_subscriber::layer::SubscriberExt;
        let subscriber = tracing_subscriber::registry().with(tracing_tracy::TracyLayer::default());
        tracing::subscriber::set_global_default(subscriber).is_ok()
    }
    #[cfg(not(feature = "profile"))]
    {
        false
    }
}

/// True when this build can profile (the `profile` feature is on).
pub const ENABLED: bool = cfg!(feature = "profile");
