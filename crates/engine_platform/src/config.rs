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
    /// Whether the initial window should be maximized while remaining windowed.
    pub maximized: bool,
    /// Minimum width in logical pixels, independent of the monitor scale.
    pub min_width: u32,
    /// Minimum height in logical pixels, independent of the monitor scale.
    pub min_height: u32,
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
            maximized: false,
            min_width: 640,
            min_height: 480,
        }
    }

    /// Requests a maximized, non-full-screen window at startup.
    pub fn maximized(mut self, maximized: bool) -> Self {
        self.maximized = maximized;
        self
    }

    /// Sets the minimum usable client area in logical pixels. Logical units
    /// keep the constraint consistent when a window moves between monitors
    /// with different scale factors.
    pub fn min_size(mut self, width: u32, height: u32) -> Self {
        self.min_width = width.max(1);
        self.min_height = height.max(1);
        self
    }
}

impl Default for WindowConfig {
    /// 1280x720, titled "VGE".
    fn default() -> Self {
        Self::new("VGE", 1280, 720)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximized_is_opt_in_and_stays_windowed() {
        assert!(!WindowConfig::default().maximized);
        assert!(
            WindowConfig::new("Studio", 1280, 720)
                .maximized(true)
                .maximized
        );
        assert_eq!(WindowConfig::default().min_width, 640);
        assert_eq!(WindowConfig::default().min_height, 480);
        assert_eq!(
            WindowConfig::new("Studio", 1280, 720)
                .min_size(0, 0)
                .min_width,
            1
        );
    }
}
