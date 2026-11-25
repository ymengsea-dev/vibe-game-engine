# Packaging Studio

`apps/studio_packager` stages a Studio distribution for the machine it runs on (macOS, Windows or Linux): the editor, the player, the Monaco code editor and the Lua language server (LuaLS 3.19.2, downloaded and checked against a pinned SHA-256). Games are never packaged.

## Build and stage

```sh
RUSTFLAGS="--remap-path-prefix=$HOME=/build" cargo build -p editor -p player --release
cargo run -p studio_packager -- target/release target/Studio [extracted-luals-dir] [options]
```

The packager refuses binaries that hold the home folder path (build with the `RUSTFLAGS` above), an output folder that already exists, and an archive that already exists. Pass an already extracted LuaLS folder as the third argument to skip the download.

| OS | Staged layout |
| --- | --- |
| macOS | `RustyEngine Studio.app` (editor and player in `Contents/MacOS`, Monaco and LuaLS in `Contents/Resources`) |
| Windows, Linux | `editor`, `player`, `monaco/` and `lua-language-server/` side by side |

## Options

| Option | What it does |
| --- | --- |
| `--archive` | Packs the output into one file beside it: `Studio.dmg` (macOS, `hdiutil`, with an Applications link to drag the app onto), `Studio.zip` (Windows, `tar -a`) or `Studio.tar.gz` (Linux) |
| `--sign-identity <id>` | macOS: signs LuaLS, the player, the editor and then the app with this Developer ID, hardened runtime and a timestamp, and signs the `.dmg` too. Without it the app is signed ad hoc (`codesign --sign -`), which runs on the Mac that built it but makes Gatekeeper warn elsewhere |
| `--notarize-profile <name>` | macOS: submits the `.dmg` with `xcrun notarytool` using this keychain profile, waits, and staples the ticket. Needs `--sign-identity` and `--archive` |
| `--windows-cert <file.pfx>` | Windows: signs `editor.exe`, `player.exe` and LuaLS with `signtool` (SHA-256, timestamped). The certificate password comes from `STUDIO_CERT_PASSWORD` and is never printed. Without it the files are unsigned |
| `--dry-run` | Stages the package but only prints the signing, archive and notarization commands |

Options that belong to another OS are refused before staging starts.

## Not verified

The macOS path has been run on a Mac. The Windows and Linux paths are covered by tests of the commands they build, but no package has been built or opened on Windows or Linux, nor on a clean machine without Rust.
