//! Engine configuration.

/// Static configuration for an [`crate::app::App`] instance.
///
/// Grows as later milestones need more startup knobs (window size, target
/// frame rate, asset root, ...). Kept minimal for the lifecycle skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineConfig {
    /// Human-readable application name (used for window titles, logging,
    /// and diagnostics in later milestones).
    pub app_name: String,
    /// Application version string, independent of the engine's own
    /// version.
    pub app_version: String,
}

impl EngineConfig {
    /// Creates a new config from an app name and version.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_core::EngineConfig;
    ///
    /// let config = EngineConfig::new("My Game", "0.1.0");
    /// assert_eq!(config.app_name, "My Game");
    /// ```
    pub fn new(app_name: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            app_name: app_name.into(),
            app_version: app_version.into(),
        }
    }
}

impl Default for EngineConfig {
    /// Default config for quick starts and tests.
    fn default() -> Self {
        Self::new("VGE Game", "0.1.0")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_name_and_version() {
        let config = EngineConfig::new("My Game", "1.2.3");
        assert_eq!(config.app_name, "My Game");
        assert_eq!(config.app_version, "1.2.3");
    }

    #[test]
    fn default_has_sensible_placeholders() {
        let config = EngineConfig::default();
        assert_eq!(config.app_name, "VGE Game");
        assert_eq!(config.app_version, "0.1.0");
    }
}
