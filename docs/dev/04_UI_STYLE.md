# Studio UI style rules

How every Studio panel, window and control looks and is built. The look is **Tokyo Night colours with a soft, minimal, Apple-like design**: filled surfaces instead of outlines, rounded shapes, quiet type and only meaningful colour. Follow these rules for any new or changed UI.

The code that implements them:

| What | Where |
| --- | --- |
| Palette, fonts, base style | `crates/engine_editor/src/theme.rs` (`NIGHT`, `Palette::apply`, `semibold`, `weighted_galley`) |
| Shared controls | `crates/engine_editor/src/widgets.rs` (`pill_button`, `toggle`, `checkbox`, `text_field`, `field`, `popup_choice`, `segmented`, `segment_label`, `more_button`, `paint_chevron`, `combo_chevron`, `one_line`) |
| Property rows | `crates/engine_editor/src/inspect.rs` (`property_row`, `cell_width`, `number`) |
| Panel frame and header | `crates/engine_editor/src/shell.rs` (`studio_panel_frame`, `panel_header`, `panel_title`, `toolbar_icon_button`) |
| Reference screens | Settings → Storage (`settings_window.rs`), the Scene panel header and footer (`shell.rs`), the Inspector (`inspector.rs`: `section`, `identity_header`) |
| Visual check | `crates/engine_editor/src/ui_snapshot.rs` (see [Checking your work](#checking-your-work)) |

---

## 1. Principles

1. **Soft surfaces, not lines.** Separate areas with a change of fill. Use a hairline only to divide rows inside a group, never to outline a control or a card.
2. **One palette.** Every colour comes from `theme::NIGHT`. Never write `Color32::from_rgb` in UI code. The only exceptions are colours that are content: a game's own UI colours, terminal ANSI colours, the 3D view, and the macOS window-control colours.
3. **Colour means something.** The accent marks focus and the active choice. The status colours mean their status. A categorical colour (for example a storage type) appears only where it identifies that category. Never use colour as decoration.
4. **Native where it counts.** On macOS, use the system font, macOS window controls, macOS-style segmented controls, toggles and pop-up buttons, and System Settings–style grouped lists.
5. **Exact layout.** Fixed widths and row heights, with text centred vertically. Text is fitted to the room the controls leave and ends in "…" before it can collide. Nothing may overflow its window or panel.
6. **Quiet type.** Regular weight for body text, semibold only for titles and headings. Hierarchy comes from size, weight and brightness, not from capitals or separator characters.

---

## 2. Colour tokens

All tokens are fields of `theme::NIGHT`. They are Tokyo Night's "Night" colours.

### Surfaces, darkest to lightest

| Token | Hex | Use |
| --- | --- | --- |
| `shell` | `#16161E` | Window background behind panels, top bar, status bar, sidebars, the track of a segmented control on a panel |
| `panel` | `#1A1B26` | Panel cards (`studio_panel_frame`), windows' content area, the segmented track on the top bar |
| `raised` | `#1F2335` | Groups and cards inside a panel (Inspector sections, Settings groups), pop-up menus |
| `hover` | `#24283B` | A hovered control; the fill of pill buttons and fields; the divider between rows in a group |
| `selected` | `#2F344D` | The chosen segment of a segmented control; a window's faint edge |

### Lines

| Token | Hex | Use |
| --- | --- | --- |
| `border` | `#232537` | The faint edge of panel cards and windows (the only outline allowed) |
| `rule` | `#26293D` | Hairline under a panel header |

### Text

| Token | Hex | Use |
| --- | --- | --- |
| `text` | `#C0CAF5` | Body text, titles, the chosen option |
| `muted` | `#8289AF` | Secondary text: descriptions, sizes, idle icons, unchosen segments |
| `subtle` | `#565F89` | Hints, notes ("Opened 41 days ago"), disabled text |

### Meaning

| Token | Hex | Use |
| --- | --- | --- |
| `accent` | `#7AA2F7` | Focus, the "on" toggle track, the active sidebar tile, primary action text ("Clean"), links |
| `accent_fill` | `#283457` | Behind a selected row (Hierarchy, sidebar item) and selected text |
| `danger` | `#F7768E` | Errors, Stop, destructive actions ("Delete" text and its tinted fill) |
| `highlight` | `#E0AF68` | Warnings, "PLAYING", and in the 3D view: the selection outline, the dragged gizmo handle and the selected-icon ring |
| `success` | `#9ECE6A` | Success, "connected" dots, good frame times |
| `info` | `#7DCFFF` | Info and debug messages; a second categorical colour |
| `purple` | `#BB9AF7` | A third categorical colour |

Status colours are always these: errors `danger`, warnings `highlight`, info `info`, success `success`. Gizmo axes use Tokyo Night red, green and blue (`gizmo::Axis::color`).

---

## 3. Typography

| Role | Font | Size | Weight |
| --- | --- | --- | --- |
| Window or page title ("Storage") | UI | 21–22 | Semibold |
| Group or section heading (project name, "Automatic cleanup") | UI | 14 | Semibold |
| Panel title ("Hierarchy", "Viewport") | UI | 12 (body style) | Semibold |
| Row title | UI | 13–13.5 | Regular |
| Body text, buttons, labels | UI | 12–12.5 | Regular |
| Description under a row, notes | UI | 11.5–12 | Regular |
| Code, logs, terminal | Monospace | 13 | Regular |

- **UI font:** SF Pro on macOS, loaded at runtime from `/System/Library/Fonts/SFNS.ttf` and drawn at its "Text" optical size. It is never bundled, because Apple's licence forbids redistributing it. Everywhere else it is the bundled Roboto.
- **Monospace:** Menlo on macOS, egui's monospace elsewhere.
- **The code editor** (Monaco) matches VS Code: `Menlo, Monaco, 'Courier New', monospace`.
- **Semibold:** use `theme::semibold("…")` for labels, or `theme::weighted_galley(…, theme::SEMIBOLD, …)` for painted text. Never set the whole UI to semibold; it flattens the hierarchy.
- **Case:** sentence case everywhere ("Clean up automatically"). No ALL-CAPS labels and no letter-spaced captions.

---

## 4. Shape and spacing

| Element | Corner radius |
| --- | --- |
| Settings-style windows | 12–14 |
| Panel cards (`studio_panel_frame`) | 10 |
| Groups and cards inside a panel | 9 |
| Segmented tracks | 9; segments 7 |
| Pill buttons, fields, sidebar items | 6–7 |
| Checkboxes (`widgets::checkbox`), egui's default widgets | 4, so a checkbox stays a box |
| Toggles, badges | fully round |

- **Spacing** is in steps of 2: 2 between segments, 6–8 between related items, 12 between controls in a row, 14–16 inside a group, 20–28 between groups, 28–32 window margins.
- **Rows** in a group are 42 points tall (one line) or 52 (title plus description).
- **Dividers** between rows start at the text, not the group's edge, as on macOS.
- **Control height:** every control in a panel row is 24 points tall: buttons, number fields, text fields, drop-downs and checkboxes. egui sizes a text edit from its text and margins only (it ignores `min_size` height), so build text fields with `widgets::text_field` or `widgets::field`, which set the margins.
- **Panel gaps:** panel cards sit 4 points apart (`PANEL_GAP_HALF` in `shell.rs`). A resizable edge shows three dots in the middle of its gap, and a 2-point `accent` line (dimmed; full while dragging) when hovered. Show resizable panels with `show_studio_collapsible`, which draws this (`paint_resize_grip`); a resizable panel shown another way calls `paint_resize_grip` itself.
- **Narrow panels:** when a panel gets too narrow for content side by side, stack the secondary part under the main one rather than squeezing both. For example, Project Assets moves its preview under the file tree below 420 points (`preview_below_tree` in `shell.rs`).

---

## 5. Components

Use the shared versions; don't restyle a control by hand.

| Need | Use | Notes |
| --- | --- | --- |
| A panel | `studio_panel_frame()`, with `panel_title(ui, icon, "Name")` or `panel_header` | The title is semibold; the header has a hairline under it |
| A choice between 2–4 options | `widgets::segmented(&NIGHT)` with `widgets::segment_label` or `toolbar_icon_button` inside | The chosen one is the lighter pill. Not tabs with underlines, and not a drop-down for two options. |
| An action button | `widgets::pill_button(ui, &NIGHT, "Clean", NIGHT.accent, enabled)` | The text colour says the kind: `accent` primary, `text` neutral, `danger` destructive |
| On/off setting | `widgets::toggle` | Not a checkbox in settings screens |
| One value from a short list | `widgets::popup_choice` ("30 days ⌄") | Not a drag field |
| Text that must fit | `widgets::one_line(…, max_width)` | Lay out the controls first, then fit the text to what is left |
| Grouped settings rows | `group` and `group_row` in `settings_window.rs` (the pattern to follow) | A title with an optional description on the left, controls in a right-aligned slot |
| A labelled property ("Position", "Body") | `inspect::property_row(ui, "Label", \|ui\| …)` | A fixed label column, so every row's controls line up; the controls wrap instead of overflowing. Below 250 points (`STACK_BELOW`) the label goes above the controls. |
| A number | `inspect::number(ui, DragValue::new(…))` | One third of the row, so single numbers line up with X / Y / Z columns. Vectors use `InspectField` for `Vec2`/`Vec3`/`Quat`: equal cells, each marked with a quiet "X", "Y" or "Z" (no axis colours). Every number field shows the drag cursor (`inspect::scrub_cursor`): a drawn left-right arrow while hovered or dragged, since dragging sideways changes the value. |
| A search field | `widgets::search_field(ui, &NIGHT, &mut text, "Filter", width)` | A soft field with a magnifier; lay the buttons out first and give it what is left |
| Tabs in the bottom panel (Console, Problems, Output, Terminal) | `chrome_tab` in `shell.rs` | The one place underlined tabs stay: the chosen tab's text and underline are `accent`. The user prefers this look for that panel; use `segmented` everywhere else. |
| A log line | `console_row` in `console.rs` (the pattern to follow) | Fixed columns: the level tag (`[WARN]` in its status colour), the source (dropped below 460 points), then the message as text and its `name=value` fields after it in quiet Menlo. Plain text: no hover fill, no row background, no dots. |
| A check box | `widgets::checkbox(ui, &NIGHT, &mut on, "Label")` | Accent-filled with a check when on. Not `ui.checkbox`. |
| A drop-down | `egui::ComboBox` with `.icon(widgets::combo_chevron)` | The trees' chevron, not egui's filled triangle |
| A group of properties | `section` in `inspector.rs` (the pattern to follow) | A soft card with a semibold title, always open (an object has only a few, so nothing folds); less-used actions go in a `widgets::more_button` "⋯" menu. A set of related rows inside it gets a small `subsection` heading. |
| Less-used actions | `widgets::more_button` with `egui::Popup::menu(&response)` | "Open script", "Remove component" (in `danger`) |
| A menu | `ui.menu_button` | Menus take the app-wide style, which is already Night |
| A window | No egui title bar. macOS window controls at the top left (red close; grey minimise and zoom when unavailable), Esc closes | See `window_controls` in `settings_window.rs` |
| A confirmation | `egui::Modal` with a `raised` frame, radius 12; actions as pill buttons on the right, the destructive one in `danger` | Name the action in the title ("Delete old engine copies?") |

---

## 6. Writing

- Use **plain, specific words** from the user's side: "Rebuilt on your next Play or Run", not internal names.
- **Buttons say what happens:** "Clean", "Show in Finder", "Delete". The same action keeps the same name everywhere.
- **Errors say what went wrong and how to fix it,** with no apology.
- **Avoid template tells:** labels joined with an em dash ("This project — island"), meta strings joined with middle dots ("1 file · …"), ALL-CAPS captions, labels above content that add nothing, and long disclaimers.

---

## 7. Avoid

- Outlines (`stroke`) on buttons, fields, segments or cards. A panel card's faint `border` is the one exception.
- Hard-coded colours, the old blue palette, or warm and brown tones (tried and rejected).
- Identical rounded cards for everything, and a "big number in a card" as the default summary. Use a centrepiece that belongs to the subject (Storage uses a segmented usage bar).
- Accent-filled backgrounds for a chosen segment (use `selected`), and accent colour as decoration.
- Content that can widen a resizable panel. A side panel keeps the width its content takes, and a vertical `ScrollArea` grows sideways to fit its widest row, so one wide row widens the panel for good. Give the scroll area a fixed rect the panel never sees past (see `inspector_tabs_panel` in `shell.rs`), and make rows wrap or fit.
- Measuring with `ui.available_width()` after other controls in a wrapping row: it returns the whole row. Use `ui.max_rect().right() - ui.cursor().min.x` for what is left.
- `CollapsingHeader` and `ui.collapsing` for groups of properties. They indent the body and draw a tree line; use `section`. Folding belongs to trees (Hierarchy, Project Assets), not to property groups.
- egui's automatic nested layouts for row content. They place items unpredictably; allocate a fixed row and position its parts.

---

## Checking your work

Look at every UI change before handing it over.

1. Add or extend a snapshot test that draws the new UI with realistic content (see `snapshot_studio_window` in `shell.rs`, or `snapshot_settings_window` in `settings_window.rs`).
2. Render and view the PNGs:

   ```sh
   VGE_SNAPSHOT_DIR=/tmp/shots cargo test -p engine_editor -- --ignored snapshot
   ```

   The snapshots include `studio.png` (the whole window), `settings.png`, `settings-automatic.png`, `scene-chrome.png`, and `inspector.png` / `inspector-narrow.png` (a wide and a narrow Inspector), `inspector-empty.png` (an object without a model), `scrub-cursor.png` (the number-field drag cursor), and `console.png` / `console-narrow.png`.
3. For a window or panel, add a containment test like `everything_in_settings_stays_inside_the_window`: no text or card outside the frame. Measure text with `TextShape::visual_bounding_rect()`.
4. Then check it in `cargo run -p editor`. The user verifies UI there.
