//! What a finished game is called and how its window opens: the "Game"
//! settings in `project.ron`, shared by Play, Run and every export.
//!
//! Every field is optional in the file, so manifests written before these
//! settings existed load unchanged and keep the old behaviour (the project
//! name as the title, a 1280 × 720 resizable window).

use serde::{Deserialize, Serialize};

/// Longest game name, in characters.
pub const MAX_GAME_NAME_CHARS: usize = 64;
/// Smallest window side a game may ask for, in pixels.
pub const MIN_WINDOW_SIDE: u32 = 320;
/// Largest window side a game may ask for, in pixels.
pub const MAX_WINDOW_SIDE: u32 = 7680;

/// The game's window when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Width in pixels (ignored while fullscreen).
    pub width: u32,
    /// Height in pixels (ignored while fullscreen).
    pub height: u32,
    /// Fill the screen instead of opening a window.
    pub fullscreen: bool,
    /// Whether players can resize the window.
    pub resizable: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fullscreen: false,
            resizable: true,
        }
    }
}

/// The project's game settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// The name players see (window title, app name). `None` uses the
    /// project's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The game's version, shown by the operating system.
    pub version: String,
    /// macOS bundle identifier. `None` makes one from the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// The icon image, a path under `assets/` (PNG or JPEG). `None` uses
    /// Studio's own icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// How the window opens.
    pub window: WindowSettings,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            name: None,
            version: "1.0.0".to_owned(),
            bundle_id: None,
            icon: None,
            window: WindowSettings::default(),
        }
    }
}

/// `#[serde(skip_serializing_if)]` predicate: untouched settings aren't
/// written, so older Studio versions keep reading the file.
pub(crate) fn is_default_game(settings: &GameSettings) -> bool {
    *settings == GameSettings::default()
}

impl GameSettings {
    /// The name players see: the set name, or `project_name`.
    #[must_use]
    pub fn display_name(&self, project_name: &str) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(project_name)
            .to_owned()
    }

    /// The bundle identifier: the set one, or `com.rustyengine.<slug>`.
    #[must_use]
    pub fn bundle_id(&self, project_name: &str) -> String {
        match self.bundle_id.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => format!(
                "com.rustyengine.{}",
                slug(&self.display_name(project_name)).to_ascii_lowercase()
            ),
        }
    }

    /// Checks the name, version, bundle id, icon path and window size.
    ///
    /// # Errors
    ///
    /// A short reason when a value is out of range.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(name) = &self.name {
            let chars = name.trim().chars().count();
            if chars > MAX_GAME_NAME_CHARS {
                return Err(format!(
                    "game name is longer than {MAX_GAME_NAME_CHARS} characters"
                ));
            }
            if name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            {
                return Err("game name can't contain / \\ : or control characters".to_owned());
            }
        }
        if self.version.trim().is_empty()
            || !self
                .version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        {
            return Err("version must be like 1.0.0".to_owned());
        }
        if let Some(id) = &self.bundle_id
            && !id.trim().is_empty()
            && !valid_bundle_id(id.trim())
        {
            return Err(
                "bundle ID must be like com.studio.game (letters, digits, - and .)".to_owned(),
            );
        }
        if let Some(icon) = &self.icon {
            let path = std::path::Path::new(icon);
            let image = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg")
                });
            let inside = path
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
            if !image || !inside {
                return Err("icon must be a PNG or JPEG inside assets/".to_owned());
            }
        }
        let side = MIN_WINDOW_SIDE..=MAX_WINDOW_SIDE;
        if !side.contains(&self.window.width) || !side.contains(&self.window.height) {
            return Err(format!(
                "window size must be {MIN_WINDOW_SIDE} to {MAX_WINDOW_SIDE} pixels each way"
            ));
        }
        Ok(())
    }
}

fn valid_bundle_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// `name` made safe as a file name on every system: letters, digits, spaces,
/// `-` and `_` kept, anything else dropped, spaces collapsed; `Game` if
/// nothing is left. A name Windows keeps for devices (`CON`, `COM1`, ...)
/// gets ` Game` added.
#[must_use]
pub fn file_safe_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect();
    let kept = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if kept.is_empty() {
        "Game".to_owned()
    } else if is_windows_device_name(&kept) {
        format!("{kept} Game")
    } else {
        kept
    }
}

/// Whether Windows reserves `name` for a device, whatever its case.
fn is_windows_device_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.len() == 1 && rest.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// `name` as one ASCII word for program names and identifiers: letters and
/// digits kept, runs of anything else become `-`; `game` if nothing is left.
#[must_use]
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "game".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_the_project_name_and_a_made_up_bundle_id() {
        let settings = GameSettings::default();
        assert_eq!(settings.display_name("Space Cats"), "Space Cats");
        assert_eq!(
            settings.bundle_id("Space Cats!"),
            "com.rustyengine.space-cats"
        );
        assert!(settings.validate().is_ok());
        let named = GameSettings {
            name: Some("  Moon  ".into()),
            bundle_id: Some("dev.me.moon".into()),
            ..settings
        };
        assert_eq!(named.display_name("x"), "Moon");
        assert_eq!(named.bundle_id("x"), "dev.me.moon");
    }

    #[test]
    fn validate_rejects_bad_values() {
        let bad = |change: fn(&mut GameSettings)| {
            let mut settings = GameSettings::default();
            change(&mut settings);
            settings.validate().is_err()
        };
        assert!(bad(|s| s.name = Some("a/b".into())));
        assert!(bad(|s| s.version = "one point oh".into()));
        assert!(bad(|s| s.bundle_id = Some("nodots".into())));
        assert!(bad(|s| s.icon = Some("../outside.png".into())));
        assert!(bad(|s| s.icon = Some("icon.gif".into())));
        assert!(bad(|s| s.window.width = 10));
        assert!(!bad(|s| s.icon = Some("ui/icon.PNG".into())));
    }

    #[test]
    fn names_become_safe_file_names_and_slugs() {
        assert_eq!(file_safe_name("Space: Cats?"), "Space Cats");
        assert_eq!(file_safe_name("..."), "Game");
        assert_eq!(file_safe_name("Con"), "Con Game");
        assert_eq!(file_safe_name("com1"), "com1 Game");
        assert_eq!(file_safe_name("Console"), "Console");
        assert_eq!(file_safe_name("COM10"), "COM10");
        assert_eq!(slug("Space Cats 2!"), "Space-Cats-2");
        assert_eq!(slug("ñ"), "game");
    }

    #[test]
    fn untouched_settings_round_trip_and_older_files_load() {
        let settings: GameSettings = ron::from_str("(window: (fullscreen: true))").unwrap();
        assert!(settings.window.fullscreen);
        assert_eq!(settings.window.width, 1280);
        assert_eq!(settings.version, "1.0.0");
        assert!(is_default_game(&GameSettings::default()));
    }
}
