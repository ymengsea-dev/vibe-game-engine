# Editor macOS and prototype parity

## Intent and acceptance

The editor should retain its real editing features while matching the supplied Scene and Code prototypes more closely. `cargo run -p editor` on macOS must open a branded app with a Dock icon and native system menu bar. Windows and Linux keep the existing in-window menus. The Scene and Code splitters shown in the prototypes must move independently within sensible minimum sizes. Egui chrome, the Code explorer, and Monaco text use the prototypes' Geist and JetBrains Mono typography. The sample game is out of scope.

## Platform and commands

Use the safe `muda` native-menu dependency only for macOS in the `editor` binary. Its menu events enter the existing editor event loop through the wake handle, then invoke shared editor commands; egui's menus invoke the same commands on Windows and Linux. Keep enabled, checked, and selected states synchronized with `EditorState`, and preserve the existing unsaved-changes guard for New/Open/Revert/Quit. Show only implemented commands and standard macOS application items; do not turn disabled placeholders into apparently functional actions. The toolbar remains in the window on every platform, but Mac omits the duplicate in-window menu labels.

macOS ignores winit's window-icon API. For Cargo launches, stage an ignored local `.app` bundle beside the built binary with an `Info.plist`, a layered-mark `.icns` derived from the supplied prototype, and an executable link to that same binary. The raw executable launches that bundle once with `open -W -n`, forwarding user arguments and the original working directory through launch environment values, then waits for the app. The bundled invocation skips relaunch. Bundle creation or launch errors are reported explicitly, not silently converted to a generic-icon window. Other platforms launch directly as before. This is local development packaging, not signing or notarization.

## Layout and typography

In Scene mode, keep Inspector full height on the right. Divide the remaining area into top and bottom rows; the top row contains an independently resizable Hierarchy and Scene viewport, and the bottom row contains an independently resizable Assets browser and Console. The row boundary, Inspector boundary, Hierarchy boundary, and Assets boundary are separate drag targets, matching the four prototype splitters. Preserve the prototype minimums: Hierarchy and Assets 160 px, Inspector 200 px, top row 140 px, and bottom row 90 px; Inspector caps at 500 px. Existing visibility toggles and panel content remain functional.

In Code mode, retain the native Monaco webview and its egui-hosted right viewport and bottom output panel. Make the explorer width inside Monaco draggable from 180 to 500 px, leaving the outer viewport (220–600 px) and output height (80–550 px) splitters independent. Keep Monaco bounds synchronized with egui while outer panels resize. This is splitter resizing, not arbitrary panel reordering.

Bundle official Geist and JetBrains Mono font files with their licenses. Egui uses Geist for proportional UI text and JetBrains Mono for monospace text. The Code webview loads the same font files locally for its explorer and Monaco editor, without a network dependency. Existing fallback families remain available for missing glyphs.

## Failure handling and verification

Do not touch project or game data when building the local app bundle. Do not overwrite an unexpected executable or icon at the bundle target. Native-menu command dispatch must be tested for state changes and guarded actions; layout sizing gets focused boundary tests where logic is pure. Verify `cargo run -p editor` on macOS visibly shows the branded Dock icon, correct native menus, working menu commands, and all Scene/Code splitter drags. Run editor-focused tests, formatting, Clippy, and available cross-platform compile checks. Report any check that cannot be run, especially Windows/Linux visual behavior or macOS signing/notarization.
