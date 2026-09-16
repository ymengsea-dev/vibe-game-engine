//! Small localization boundary for editor chrome.
//!
//! The editor currently ships English text only, but stable keys keep future
//! translation catalogs separate from panel behavior and persistence.

/// Locales understood by the editor shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    /// Built-in catalog.
    #[default]
    English,
}

/// Stable keys for labels shared by the editor chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKey {
    /// Window menu label.
    Window,
    /// Asset browser panel label.
    AssetBrowser,
    /// Inspector panel label.
    Inspector,
    /// Hierarchy panel label.
    Hierarchy,
    /// Scene panel label.
    Scene,
    /// Console panel label.
    Console,
}

impl TextKey {
    /// Returns the localized label, falling back to English when a catalog
    /// does not contain a key.
    pub const fn label(self, _locale: Locale) -> &'static str {
        match self {
            Self::Window => "Window",
            Self::AssetBrowser => "Asset Browser",
            Self::Inspector => "Inspector / Lighting",
            Self::Hierarchy => "Hierarchy",
            Self::Scene => "Scene",
            Self::Console => "Console / Problems / Output",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_catalog_has_non_empty_stable_labels() {
        for key in [
            TextKey::Window,
            TextKey::AssetBrowser,
            TextKey::Inspector,
            TextKey::Hierarchy,
            TextKey::Scene,
            TextKey::Console,
        ] {
            assert!(!key.label(Locale::English).is_empty());
        }
    }
}
