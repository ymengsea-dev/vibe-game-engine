//! Window configuration.

/// Configuration for the window created by [`crate::run_windowed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowConfig {
    /// Window title bar text.
    pub title: String,
    /// Initial width in logical pixels.
    pub width: u32,
    /// Initial height in logical pixels.
    pub height: u32,
}

impl WindowConfig {
    /// Creates a new window config.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_platform::WindowConfig;
    ///
    /// let config = WindowConfig::new("My Game", 1280, 720);
    /// assert_eq!(config.width, 1280);
    /// ```
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            title: title.into(),
            width,
            height,
        }
    }
}

impl Default for WindowConfig {
    /// 1280x720, titled "VGE".
    fn default() -> Self {
        Self::new("VGE", 1280, 720)
    }
}
