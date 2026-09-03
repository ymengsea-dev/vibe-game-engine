//! Project-wide search: the bottom panel's **Search** tab.
//!
//! Two modes. *Text* is a literal substring scan over the project's
//! source files ([`run_text_search`], pure and testable — the walk skips
//! `target/`, VCS, and binary-looking files). *Symbols* forwards the
//! query to rust-analyzer's `workspace/symbol` and lists what comes
//! back; the host owns that round-trip and drops the results into
//! [`SearchState::symbol_results`].
//!
//! Clicking a result sets [`SearchState::jump`]; the host opens the file
//! and reveals the location (reusing the code editor's `reveal`).
//!
//! Text search reports **one hit per matching line** (the first match on
//! it), like `grep` without `-o` — enough to navigate to the line.

use std::fs;
use std::path::{Path, PathBuf};

/// File extensions [`run_text_search`] looks inside.
const SEARCH_EXTENSIONS: [&str; 8] = ["rs", "toml", "ron", "wgsl", "glsl", "md", "txt", "json"];

/// Directory names the walk never descends into.
const SKIP_DIRS: [&str; 3] = ["target", ".git", ".studio"];

/// How deep the walk recurses — bounds pathological nesting and symlink
/// cycles (the walk follows symlinks, so this is the only cycle guard).
const MAX_SEARCH_DEPTH: u32 = 24;

/// Hard cap on text hits per search — the panel shows a "truncated" note
/// when it's reached.
pub const MAX_TEXT_HITS: usize = 1000;

/// Which kind of search the Search tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Literal substring over source files.
    #[default]
    Text,
    /// rust-analyzer `workspace/symbol`.
    Symbols,
}

/// The query and its match options (literal only — no regex yet).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// The text to look for.
    pub text: String,
    /// Match case exactly.
    pub case_sensitive: bool,
    /// Require a non-word character (or edge) on both sides of the match.
    pub whole_word: bool,
}

/// One text-search match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHit {
    /// Project-root-relative path of the file.
    pub path: PathBuf,
    /// 1-based line.
    pub line: u32,
    /// 1-based column (character count).
    pub col: u32,
    /// The matching line, leading whitespace trimmed, clamped to 200
    /// chars.
    pub preview: String,
}

/// The result of [`run_text_search`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextResults {
    /// Matches, in file-path then line order.
    pub hits: Vec<TextHit>,
    /// Whether the hit cap was reached and results are incomplete.
    pub truncated: bool,
}

/// One symbol-search match, converted by the host from
/// `editor_lsp`'s reply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SymbolHit {
    /// The symbol's name.
    pub name: String,
    /// LSP `SymbolKind` as its raw number (see [`symbol_kind_label`]).
    pub kind: u8,
    /// Enclosing scope (module / type), if the server gave one.
    pub container: Option<String>,
    /// Project-root-relative path.
    pub path: PathBuf,
    /// 1-based line (0 becomes 1 if the server gave no range).
    pub line: u32,
    /// 1-based column.
    pub col: u32,
}

/// A location the user asked to jump to (drained by the host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jump {
    /// Project-root-relative path.
    pub path: PathBuf,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub col: u32,
}

/// Everything the Search tab reads and writes. On [`EditorState`]
/// (`state.search`).
///
/// [`EditorState`]: crate::EditorState
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Text or Symbols.
    pub mode: SearchMode,
    /// The current query.
    pub query: SearchQuery,
    /// Latest text-search results.
    pub text_results: TextResults,
    /// Latest symbol-search results (filled by the host from the LSP
    /// reply).
    pub symbol_results: Vec<SymbolHit>,
    /// Set when the user submits a text search; the host runs
    /// [`run_text_search`] and clears it.
    pub run_text_requested: bool,
    /// Set when the query changed in Symbols mode; the host sends a
    /// `workspace/symbol` request and clears it.
    pub symbol_query_dirty: bool,
    /// A result the user clicked; the host opens + reveals it, then
    /// clears it.
    pub jump: Option<Jump>,
}

/// A short label for an LSP `SymbolKind` number.
pub fn symbol_kind_label(kind: u8) -> &'static str {
    match kind {
        2 => "mod",
        5 => "class",
        6 => "method",
        7 => "property",
        8 => "field",
        9 => "ctor",
        10 => "enum",
        11 => "trait",
        12 => "fn",
        13 => "var",
        14 => "const",
        22 => "variant",
        23 => "struct",
        24 => "event",
        26 => "type",
        _ => "symbol",
    }
}

/// Scans every source file under `root` for `query`, returning at most
/// `limit` hits (after which [`TextResults::truncated`] is set).
///
/// Skips `target/` / `.git/` / `.studio/`, files without a known source
/// extension, files whose first kilobyte contains a NUL byte (binary),
/// and non-UTF-8 files. An empty query, or a `root` that can't be read,
/// yields an empty result rather than an error.
pub fn run_text_search(root: &Path, query: &SearchQuery, limit: usize) -> TextResults {
    let mut results = TextResults::default();
    if query.text.is_empty() {
        return results;
    }
    let needle = if query.case_sensitive {
        query.text.clone()
    } else {
        query.text.to_lowercase()
    };
    walk(root, root, 0, query, &needle, limit, &mut results);
    results
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: u32,
    query: &SearchQuery,
    needle: &str,
    limit: usize,
    results: &mut TextResults,
) {
    if results.truncated || depth >= MAX_SEARCH_DEPTH {
        return;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = read_dir.flatten().map(|entry| entry.path()).collect();
    paths.sort();

    for path in paths {
        if results.truncated {
            return;
        }
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SKIP_DIRS.contains(&name));
            if !skip {
                walk(root, &path, depth + 1, query, needle, limit, results);
            }
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| SEARCH_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
        if !ext_ok {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        if bytes.iter().take(1024).any(|&byte| byte == 0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

        for (index, line) in text.lines().enumerate() {
            if let Some(col) = match_line(line, query, needle) {
                results.hits.push(TextHit {
                    path: relative.clone(),
                    line: index as u32 + 1,
                    col: col as u32,
                    preview: line.trim_start().chars().take(200).collect(),
                });
                if results.hits.len() >= limit {
                    results.truncated = true;
                    return;
                }
            }
        }
    }
}

/// The 1-based column of the first `needle` match on `line` honouring
/// `query`'s options, or `None`.
fn match_line(line: &str, query: &SearchQuery, needle: &str) -> Option<usize> {
    let haystack = if query.case_sensitive {
        line.to_string()
    } else {
        line.to_lowercase()
    };
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(needle) {
        let start = from + offset;
        let end = start + needle.len();
        if !query.whole_word || word_bounded(&haystack, start, end) {
            return Some(haystack[..start].chars().count() + 1);
        }
        from = start + 1;
    }
    None
}

fn word_bounded(text: &str, start: usize, end: usize) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before_ok = start == 0 || !text[..start].chars().next_back().is_some_and(is_word);
    let after_ok = end >= text.len() || !text[end..].chars().next().is_some_and(is_word);
    before_ok && after_ok
}

/// Draws the Search tab. Sets [`SearchState::run_text_requested`] /
/// [`SearchState::symbol_query_dirty`] on submit and
/// [`SearchState::jump`] on a result click — the host acts on those.
pub fn show(ui: &mut egui::Ui, state: &mut SearchState) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.mode, SearchMode::Text, "Text");
        ui.selectable_value(&mut state.mode, SearchMode::Symbols, "Symbols");
    });

    let text_mode = state.mode == SearchMode::Text;
    let submit = ui
        .horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut state.query.text)
                    .hint_text("Search project…")
                    .desired_width(240.0),
            );
            let entered = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = ui.button("Search").clicked();
            if text_mode {
                ui.checkbox(&mut state.query.case_sensitive, "Aa")
                    .on_hover_text("Match case");
                ui.checkbox(&mut state.query.whole_word, "W")
                    .on_hover_text("Whole word");
            } else if response.changed() {
                state.symbol_query_dirty = true;
            }
            entered || clicked
        })
        .inner;

    if submit {
        if text_mode {
            state.run_text_requested = true;
        } else {
            state.symbol_query_dirty = true;
        }
    }

    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if text_mode {
                show_text_results(ui, state);
            } else {
                show_symbol_results(ui, state);
            }
        });
}

fn show_text_results(ui: &mut egui::Ui, state: &mut SearchState) {
    let results = &state.text_results;
    ui.horizontal(|ui| {
        ui.label(format!("{} results", results.hits.len()));
        if results.truncated {
            ui.weak(format!("(showing the first {MAX_TEXT_HITS})"));
        }
    });
    if results.hits.is_empty() {
        ui.weak("No matches. Type a query and press Enter.");
        return;
    }

    let mut jump = None;
    let mut current_file: Option<&Path> = None;
    for hit in &results.hits {
        if current_file != Some(hit.path.as_path()) {
            current_file = Some(hit.path.as_path());
            ui.add_space(4.0);
            ui.strong(hit.path.display().to_string());
        }
        let label = format!("{:>5}  {}", hit.line, hit.preview);
        if ui.selectable_label(false, label).clicked() {
            jump = Some(Jump {
                path: hit.path.clone(),
                line: hit.line,
                col: hit.col,
            });
        }
    }
    if jump.is_some() {
        state.jump = jump;
    }
}

fn show_symbol_results(ui: &mut egui::Ui, state: &mut SearchState) {
    ui.label(format!("{} symbols", state.symbol_results.len()));
    if state.symbol_results.is_empty() {
        ui.weak("No symbols. Type a name (rust-analyzer must be running).");
        return;
    }

    let mut jump = None;
    for symbol in &state.symbol_results {
        let mut label = format!("{:<8} {}", symbol_kind_label(symbol.kind), symbol.name);
        if let Some(container) = &symbol.container
            && !container.is_empty()
        {
            label.push_str(&format!("   ({container})"));
        }
        label.push_str(&format!("   {}", symbol.path.display()));
        if ui.selectable_label(false, label).clicked() {
            jump = Some(Jump {
                path: symbol.path.clone(),
                line: symbol.line,
                col: symbol.col,
            });
        }
    }
    if jump.is_some() {
        state.jump = jump;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vge-search-test-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn finds_matches_with_line_column_and_preview() {
        let dir = temp_dir("basic");
        std::fs::write(
            dir.join("a.rs"),
            "fn main() {}\n    let needle = 1;\nfn other() {}\n",
        )
        .unwrap();

        let results = run_text_search(&dir, &query("needle"), 100);
        assert_eq!(results.hits.len(), 1);
        let hit = &results.hits[0];
        assert_eq!(hit.path, Path::new("a.rs"));
        assert_eq!(hit.line, 2);
        assert_eq!(hit.col, 9); // after the 8 leading chars "    let "
        assert_eq!(hit.preview, "let needle = 1;");
        assert!(!results.truncated);
    }

    #[test]
    fn case_and_whole_word_options_apply() {
        let dir = temp_dir("opts");
        // One match reported per line (like `grep` without `-o`).
        std::fs::write(dir.join("a.rs"), "Foo\nfoobar\nFOO\nsome foo here\n").unwrap();

        // Case-insensitive, any position: all four lines match.
        assert_eq!(run_text_search(&dir, &query("foo"), 100).hits.len(), 4);

        let cased = SearchQuery {
            text: "Foo".into(),
            case_sensitive: true,
            whole_word: false,
        };
        assert_eq!(run_text_search(&dir, &cased, 100).hits.len(), 1);

        let word = SearchQuery {
            text: "foo".into(),
            case_sensitive: false,
            whole_word: true,
        };
        // Lines 1 ("Foo"), 3 ("FOO"), 4 ("foo") are whole words; line 2
        // ("foobar") is not.
        assert_eq!(run_text_search(&dir, &word, 100).hits.len(), 3);
    }

    #[test]
    fn skips_target_dir_binary_and_unknown_extensions() {
        let dir = temp_dir("skips");
        std::fs::write(dir.join("keep.rs"), "hit here\n").unwrap();
        std::fs::write(dir.join("image.png"), "hit but wrong ext\n").unwrap();
        std::fs::write(dir.join("binary.rs"), "hit\0with nul\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target").join("gen.rs"), "hit in target\n").unwrap();

        let results = run_text_search(&dir, &query("hit"), 100);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].path, Path::new("keep.rs"));
    }

    #[test]
    fn caps_at_the_limit_and_marks_truncated() {
        let dir = temp_dir("cap");
        let body: String = (0..50).map(|_| "x\n").collect();
        std::fs::write(dir.join("a.rs"), &body).unwrap();

        let results = run_text_search(&dir, &query("x"), 10);
        assert_eq!(results.hits.len(), 10);
        assert!(results.truncated);
    }

    #[test]
    fn empty_query_or_missing_root_is_empty() {
        let dir = temp_dir("empty");
        assert!(run_text_search(&dir, &query(""), 100).hits.is_empty());
        assert!(
            run_text_search(Path::new("/no/such/dir"), &query("x"), 100)
                .hits
                .is_empty()
        );
    }

    #[test]
    fn symbol_kind_label_covers_the_common_rust_kinds() {
        assert_eq!(symbol_kind_label(12), "fn");
        assert_eq!(symbol_kind_label(23), "struct");
        assert_eq!(symbol_kind_label(11), "trait");
        assert_eq!(symbol_kind_label(99), "symbol");
    }
}
