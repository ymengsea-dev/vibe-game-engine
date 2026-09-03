//! Integrated terminal: a real pseudo-terminal running the user's shell,
//! its screen parsed by `alacritty_terminal` and painted as an egui
//! character grid.
//!
//! Compiled only with the `terminal` feature so the PTY / VT stack never
//! reaches a runtime build (`game` -> `engine` -> `engine_editor`); see
//! this crate's `Cargo.toml` and NFR-004.
//!
//! [`Terminal::spawn`] opens the PTY, starts the shell in a given
//! directory, and launches a reader thread that feeds every byte from
//! the shell into the terminal model. [`show`] measures a monospace
//! cell, resizes the PTY to fit the panel, paints the visible grid, and
//! forwards keyboard / wheel input while the widget has focus.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Background colour painted behind the whole grid; cells with this exact
/// background skip their own fill.
const TERMINAL_BG: egui::Color32 = egui::Color32::from_rgb(0x12, 0x12, 0x16);
/// Foreground used for the default text colour and the cursor.
const TERMINAL_FG: egui::Color32 = egui::Color32::from_rgb(0xcc, 0xcc, 0xcc);
/// Grid the PTY and model start at, before the first [`show`] resize.
const INITIAL: GridSize = GridSize { cols: 80, rows: 24 };
/// Point size of the monospace font the grid is drawn in.
const FONT_SIZE: f32 = 13.0;

type SharedTerm = Arc<Mutex<Term<EventProxy>>>;
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Why a [`Terminal`] could not be created.
#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    /// The OS refused to allocate a pseudo-terminal.
    #[error("could not open a pseudo-terminal: {0}")]
    OpenPty(String),
    /// The shell process would not start.
    #[error("could not start the shell: {0}")]
    Spawn(String),
    /// The pseudo-terminal's reader/writer could not be obtained, or the
    /// reader thread failed to spawn.
    #[error("pseudo-terminal I/O error: {0}")]
    Io(String),
}

/// A grid size in character cells. Doubles as the `alacritty_terminal`
/// [`Dimensions`] the model is sized against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Sink for terminal-driven events. The only one that matters here is
/// [`TermEvent::PtyWrite`] — replies the model needs to send back up the
/// PTY (e.g. answers to device-status queries).
#[derive(Clone)]
struct EventProxy {
    writer: SharedWriter,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        if let TermEvent::PtyWrite(text) = event
            && let Ok(mut writer) = self.writer.lock()
        {
            let _ = writer.write_all(text.as_bytes());
            let _ = writer.flush();
        }
    }
}

/// A shell running in a pseudo-terminal, with its screen model and the
/// thread that keeps the model fed.
pub struct Terminal {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: SharedWriter,
    term: SharedTerm,
    size: GridSize,
    reader_alive: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Terminal {
    /// Opens a pseudo-terminal, starts the user's default shell with its
    /// working directory set to `cwd`, and begins reading its output.
    ///
    /// # Errors
    ///
    /// [`TerminalError::OpenPty`] if the OS won't allocate a PTY,
    /// [`TerminalError::Spawn`] if the shell won't start, or
    /// [`TerminalError::Io`] if the PTY's reader/writer or the reader
    /// thread can't be set up.
    pub fn spawn(cwd: &Path) -> Result<Self, TerminalError> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: INITIAL.rows as u16,
                cols: INITIAL.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| TerminalError::OpenPty(err.to_string()))?;

        let mut command = CommandBuilder::new_default_prog();
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| TerminalError::Spawn(err.to_string()))?;
        // Drop our handle to the slave: once the child exits, the PTY
        // then has no writers and the reader sees EOF.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| TerminalError::Io(err.to_string()))?;
        let writer: SharedWriter = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|err| TerminalError::Io(err.to_string()))?,
        ));

        let term: SharedTerm = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &INITIAL,
            EventProxy {
                writer: Arc::clone(&writer),
            },
        )));

        let reader_alive = Arc::new(AtomicBool::new(true));
        let reader = spawn_reader(reader, Arc::clone(&term), Arc::clone(&reader_alive))
            .map_err(|err| TerminalError::Io(err.to_string()))?;

        Ok(Self {
            master: pair.master,
            child,
            writer,
            term,
            size: INITIAL,
            reader_alive,
            reader: Some(reader),
        })
    }

    /// Whether the shell process is still alive.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Resizes the PTY and the model to `cols` x `rows` cells (clamped to
    /// at least 1x1). A no-op when the size is unchanged.
    fn resize(&mut self, cols: usize, rows: usize) {
        let next = GridSize {
            cols: cols.max(1),
            rows: rows.max(1),
        };
        if next == self.size {
            return;
        }
        self.size = next;
        let _ = self.master.resize(PtySize {
            rows: next.rows as u16,
            cols: next.cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut term) = self.term.lock() {
            term.resize(next);
        }
    }

    /// Writes raw bytes to the shell's input.
    fn send(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Scrolls the viewport by `lines` (positive scrolls up into
    /// scrollback).
    fn scroll(&self, lines: i32) {
        if lines == 0 {
            return;
        }
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Delta(lines));
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.reader_alive.store(false, Ordering::SeqCst);
        // Killing the child closes the PTY, so the reader's blocking
        // `read` returns 0 and the thread leaves its loop.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Spawns the thread that pumps PTY output into `term` until EOF or
/// `alive` is cleared.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    term: SharedTerm,
    alive: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("studio-terminal-reader".to_owned())
        .spawn(move || {
            let mut processor: Processor = Processor::new();
            let mut buffer = [0u8; 8192];
            while alive.load(Ordering::SeqCst) {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if let Ok(mut term) = term.lock() {
                            processor.advance(&mut *term, &buffer[..read]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
}

/// Draws the terminal into the remaining space of `ui` and, while the
/// widget holds focus, forwards typed text, control keys, and wheel
/// scrolling to the shell.
pub fn show(ui: &mut egui::Ui, terminal: &mut Terminal) {
    if !terminal.is_running() {
        ui.horizontal(|ui| {
            ui.colored_label(
                egui::Color32::from_rgb(0xdc, 0xb4, 0x5a),
                "Shell exited — reopen the tab to start a new one.",
            );
        });
    }

    let font_id = egui::FontId::monospace(FONT_SIZE);
    let sample = ui
        .painter()
        .layout_no_wrap("M".to_owned(), font_id.clone(), TERMINAL_FG);
    let cell_w = sample.size().x.max(1.0);
    let cell_h = sample.size().y.max(1.0);

    let available = ui.available_size();
    let cols = (available.x / cell_w).floor().max(1.0) as usize;
    let rows = (available.y / cell_h).floor().max(1.0) as usize;
    terminal.resize(cols, rows);

    let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
    if response.clicked() {
        response.request_focus();
    }
    let focused = response.has_focus();

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, TERMINAL_BG);

    let app_cursor = {
        let Ok(term) = terminal.term.lock() else {
            return;
        };
        let content = term.renderable_content();
        let scrolled_back = content.display_offset != 0;
        let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR);

        for indexed in content.display_iter {
            let row = indexed.point.line.0;
            let col = indexed.point.column.0;
            if row < 0 {
                continue;
            }
            let origin = egui::pos2(
                rect.min.x + col as f32 * cell_w,
                rect.min.y + row as f32 * cell_h,
            );
            let cell = indexed.cell;

            let mut fg = ansi_color(cell.fg, TERMINAL_FG);
            let mut bg = ansi_color(cell.bg, TERMINAL_BG);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.gamma_multiply(0.7);
            }

            if bg != TERMINAL_BG {
                painter.rect_filled(
                    egui::Rect::from_min_size(origin, egui::vec2(cell_w, cell_h)),
                    0.0,
                    bg,
                );
            }
            if cell.c != ' ' && cell.c != '\0' && !cell.flags.contains(Flags::HIDDEN) {
                painter.text(origin, egui::Align2::LEFT_TOP, cell.c, font_id.clone(), fg);
            }
        }

        if show_cursor && !scrolled_back {
            let cursor = content.cursor.point;
            if cursor.line.0 >= 0 {
                let cursor_rect = egui::Rect::from_min_size(
                    egui::pos2(
                        rect.min.x + cursor.column.0 as f32 * cell_w,
                        rect.min.y + cursor.line.0 as f32 * cell_h,
                    ),
                    egui::vec2(cell_w, cell_h),
                );
                painter.rect_filled(
                    cursor_rect,
                    0.0,
                    TERMINAL_FG.gamma_multiply(if focused { 0.55 } else { 0.25 }),
                );
            }
        }

        content.mode.contains(TermMode::APP_CURSOR)
    };

    if focused {
        for event in ui.input(|input| input.events.clone()) {
            match event {
                egui::Event::Text(text) => terminal.send(text.as_bytes()),
                egui::Event::Paste(text) => terminal.send(text.as_bytes()),
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                        terminal.send(&bytes);
                    }
                }
                _ => {}
            }
        }
        let wheel = ui.input(|input| input.smooth_scroll_delta.y);
        if wheel.abs() >= cell_h {
            terminal.scroll((wheel / cell_h).round() as i32);
        }
    }
}

/// Translates a key press to the bytes a terminal expects. Returns
/// `None` for plain printable keys — those arrive as [`egui::Event::Text`]
/// and are sent verbatim.
fn encode_key(key: egui::Key, modifiers: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key;

    let ctrl_only = (modifiers.ctrl || modifiers.command) && !modifiers.shift && !modifiers.alt;
    if ctrl_only {
        // Ctrl + A..Z -> 0x01..0x1A, plus the [ \ ] group.
        let letter = |base: u8| Some(vec![base]);
        return match key {
            Key::A => letter(0x01),
            Key::B => letter(0x02),
            Key::C => letter(0x03),
            Key::D => letter(0x04),
            Key::E => letter(0x05),
            Key::F => letter(0x06),
            Key::G => letter(0x07),
            Key::H => letter(0x08),
            Key::I => letter(0x09),
            Key::J => letter(0x0a),
            Key::K => letter(0x0b),
            Key::L => letter(0x0c),
            Key::M => letter(0x0d),
            Key::N => letter(0x0e),
            Key::O => letter(0x0f),
            Key::P => letter(0x10),
            Key::Q => letter(0x11),
            Key::R => letter(0x12),
            Key::S => letter(0x13),
            Key::T => letter(0x14),
            Key::U => letter(0x15),
            Key::V => letter(0x16),
            Key::W => letter(0x17),
            Key::X => letter(0x18),
            Key::Y => letter(0x19),
            Key::Z => letter(0x1a),
            Key::OpenBracket => letter(0x1b),
            Key::Backslash => letter(0x1c),
            Key::CloseBracket => letter(0x1d),
            _ => None,
        };
    }

    let csi = |tail: &str| Some(format!("\x1b[{tail}").into_bytes());
    let arrow = |final_byte: char| {
        let lead = if app_cursor { 'O' } else { '[' };
        Some(format!("\x1b{lead}{final_byte}").into_bytes())
    };

    match key {
        Key::Enter => Some(vec![b'\r']),
        Key::Tab => Some(vec![b'\t']),
        Key::Backspace => Some(vec![0x7f]),
        Key::Escape => Some(vec![0x1b]),
        Key::ArrowUp => arrow('A'),
        Key::ArrowDown => arrow('B'),
        Key::ArrowRight => arrow('C'),
        Key::ArrowLeft => arrow('D'),
        Key::Home => csi("H"),
        Key::End => csi("F"),
        Key::Insert => csi("2~"),
        Key::Delete => csi("3~"),
        Key::PageUp => csi("5~"),
        Key::PageDown => csi("6~"),
        _ => None,
    }
}

/// Maps an `alacritty_terminal` cell colour to an egui colour, using
/// `default` for the terminal's default fg/bg named colours.
fn ansi_color(color: AnsiColor, default: egui::Color32) -> egui::Color32 {
    match color {
        AnsiColor::Spec(rgb) => egui::Color32::from_rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(index) => indexed_color(index),
        AnsiColor::Named(named) => match named {
            NamedColor::Foreground | NamedColor::BrightForeground => TERMINAL_FG,
            NamedColor::Background => TERMINAL_BG,
            NamedColor::Cursor => TERMINAL_FG,
            NamedColor::DimForeground => TERMINAL_FG.gamma_multiply(0.7),
            other => {
                let base = other as usize;
                if base < 16 {
                    indexed_color(base as u8)
                } else {
                    default
                }
            }
        },
    }
}

/// The xterm 256-colour palette: 0..15 the ANSI set, 16..231 a 6x6x6
/// cube, 232..255 a 24-step grey ramp.
fn indexed_color(index: u8) -> egui::Color32 {
    const ANSI16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x31, 0x31),
        (0x0d, 0xbc, 0x79),
        (0xe5, 0xe5, 0x10),
        (0x24, 0x72, 0xc8),
        (0xbc, 0x3f, 0xbc),
        (0x11, 0xa8, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x66, 0x66, 0x66),
        (0xf1, 0x4c, 0x4c),
        (0x23, 0xd1, 0x8b),
        (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea),
        (0xd6, 0x70, 0xd6),
        (0x29, 0xb8, 0xdb),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => {
            let (r, g, b) = ANSI16[index as usize];
            egui::Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let i = index - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            egui::Color32::from_rgb(step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            egui::Color32::from_rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(encode_key(Key::C, Modifiers::CTRL, false), Some(vec![0x03]));
        assert_eq!(encode_key(Key::D, Modifiers::CTRL, false), Some(vec![0x04]));
        assert_eq!(
            encode_key(Key::OpenBracket, Modifiers::CTRL, false),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn plain_letters_defer_to_text_events() {
        assert_eq!(encode_key(Key::A, Modifiers::NONE, false), None);
    }

    #[test]
    fn arrows_follow_the_application_cursor_mode() {
        assert_eq!(
            encode_key(Key::ArrowUp, Modifiers::NONE, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, Modifiers::NONE, true),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn named_control_keys_encode() {
        assert_eq!(
            encode_key(Key::Enter, Modifiers::NONE, false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            encode_key(Key::Backspace, Modifiers::NONE, false),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_key(Key::Delete, Modifiers::NONE, false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(Key::Escape, Modifiers::NONE, false),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn indexed_palette_covers_every_band() {
        assert_eq!(indexed_color(0), egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed_color(15), egui::Color32::from_rgb(0xff, 0xff, 0xff));
        // 16 is the black corner of the cube; 231 is the white corner.
        assert_eq!(indexed_color(16), egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(indexed_color(231), egui::Color32::from_rgb(255, 255, 255));
        // Grey ramp endpoints.
        assert_eq!(indexed_color(232), egui::Color32::from_rgb(8, 8, 8));
        assert_eq!(indexed_color(255), egui::Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn grid_size_reports_itself_as_dimensions() {
        let size = GridSize {
            cols: 120,
            rows: 40,
        };
        assert_eq!(size.columns(), 120);
        assert_eq!(size.screen_lines(), 40);
        assert_eq!(size.total_lines(), 40);
    }

    #[test]
    #[ignore = "spawns a real shell; run locally with --ignored"]
    fn spawn_runs_a_command_and_reaps_on_drop() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir();
        let terminal = Terminal::spawn(&dir).expect("spawn a shell");
        terminal.send(b"printf ready\n");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_output = false;
        while Instant::now() < deadline {
            if let Ok(term) = terminal.term.lock() {
                let text: String = term
                    .renderable_content()
                    .display_iter
                    .map(|c| c.cell.c)
                    .collect();
                if text.contains("ready") {
                    saw_output = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(saw_output, "expected the shell to echo `ready`");

        drop(terminal);
    }
}
