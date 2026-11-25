# RustyEngine Studio — Exporting Your Game

This guide covers how to turn your project into a game other people can start without Studio, Lua or Rust installed: what the game is called, its icon, how its window opens, and how to make and try it.

---

## Game settings

Open **File → Settings…** (⌘,), then **Project**. The **Game** group holds what the finished game looks like to players. Studio saves a change as soon as it is valid; if it isn't, a red line under the group says why and nothing is saved.

| Setting | What it does |
| --- | --- |
| **Name** | The window title and the app's name. Empty uses the project's name. It also names the exported app or folder (characters like `/` and `:` are left out of file names) |
| **Version** | Shown by the system, for example in Finder's Get Info. Like `1.0.0` |
| **Bundle ID** | macOS only: the app's identity, like `com.yourname.yourgame`. Empty makes one from the name. Keep it the same between versions so macOS treats updates as the same app |
| **Icon** | A PNG or JPEG from your project. A square image of 1024 × 1024 pixels looks best; others are centred on a clear square. **Studio icon** uses Studio's own |
| **Window size** | How big the window opens, in points (320 to 7680 each way) |
| **Full screen** | Fill the screen instead of opening a window |
| **Resizable window** | Whether players can drag the window's edges |

These settings are used by **Run** and by every export. **Play** in the Scene View ignores the window settings, because the game draws inside Studio there.

A Rust game's own code can still set a title and size; the Game settings win when the game is run from Studio or exported.

---

## Build & Export…

**File → Build & Export…** asks for a folder and puts the game in it. It uses the configuration selected in the toolbar (Debug or Release). A Rust game is compiled first, which can take a few minutes the first time.

What you get depends on the system Studio runs on:

| System | What appears in the folder |
| --- | --- |
| macOS | **`<Name>.app`**, with your icon and name. Double-click it to play |
| Windows | A **`<Name>`** folder with **`<Name>.exe`** and the game's data files |
| Linux | A **`<Name>`** folder with the program and the game's data files |

Send players the whole `.app`, or the whole folder (zip it first). The program can't run without the files beside it.

Exporting again into the same folder replaces the earlier export of the same project. Studio never replaces anything else: if the folder already holds a file or folder with that name that this project didn't make, the export stops and says so. If an export fails half way, the earlier one stays as it was.

Progress and compiler messages appear in the **Output** tab, lines starting with `[export]`.

---

## Build and Run (⌘B)

**File → Build and Run** (⌘B) exports the selected configuration into the project's **`Builds/<configuration>`** folder and starts the game straight away, exactly as a player would get it. Messages the game prints appear in the Output tab as `[game]`.

The `Builds` folder can be cleaned in **Settings → Storage** under **Game builds**; the next Build and Run makes it again. If your project is in Git, you may want to add `Builds/` to `.gitignore`.

---

## Sharing a Mac app

Exported Mac apps are signed only for your own Mac (an ad-hoc signature), not with an Apple Developer ID. On another Mac, the first time a player opens it, macOS warns that it can't check the app. They can open it with a right-click (or Control-click) on the app → **Open** → **Open**, or in **System Settings → Privacy & Security** → **Open Anyway**.

---

## Limits

- You can export only for the system Studio runs on: a Mac makes Mac apps, Windows makes Windows games.
- The Windows `.exe` itself shows the standard program icon in Explorer; the icon appears on the game's window and taskbar button.
- Windows and Linux exports have not yet been tried on those systems. Studio supports macOS for now.
- Mac apps aren't notarised, so other Macs show the warning above.
