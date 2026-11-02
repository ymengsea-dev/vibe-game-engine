# RustyEngine Studio — Storage

Making games leaves files on your disk that Studio can rebuild whenever it needs them, mostly compiled code. This guide explains what they are, how to see their size, and how to clean them up, by hand or automatically.

Open **File → Settings…** (or press **⌘,**). Storage is the first section.

---

## What Studio can clean

| Item | What it is | When it is safe to delete |
| --- | --- | --- |
| **Build cache** | The compiled code of a **Rust** game, in the project's `target` folder. Usually the biggest item: a few GB per project. Lua projects have none. | Always. The next **Play** or **Run** rebuilds it; that one start takes longer (a few minutes after a full clean). |
| **Temp files** | Studio's leftovers: the Play staging folder, and crash reports older than 30 days. | Always. |
| **Old engine copies** | Engine folders under `sdk/` that **Upgrade Rust SDK** left behind and the project no longer uses. | Always. Studio asks once before deleting them. |

Studio also lists **Backups**: copies of your files that Studio saved before changing them automatically, such as `project.ron.v1.bak` (before upgrading the project format) or `Cargo.toml.sdk-<version>.bak` (before **Upgrade Rust SDK**). They are tiny, and they let you undo a change Studio made. Studio never deletes them. Use **Show in Finder** if you want to remove them yourself.

**Never touched:** your scenes, assets, scripts and code, your asset IDs (`.studio/assets`, which every scene relies on), your editor layout, the backup of unsaved changes, and game saves.

---

## The Storage section

- At the top, the total and a **storage bar** show how the space splits between build caches, temp files, old engine copies and backups. Each kind has its own colour, and the dots under the bar name them.
- Below it, each project you opened in Studio has its own list, starting with the open one ("Open now"; the others show when you last opened them). Each item shows its size, with a **Clean** button for each item Studio can delete.

Sizes are measured in the background, so the window never freezes Studio. Cleaning the open project's build cache is not possible while **Play**, **Run** or **Build & Export** is running, because the build is using it.

Studio only ever looks at projects you opened in Studio. It never searches your disk.

---

## Automatic cleanup

Off by default. Turn on **Clean up automatically** to let Studio keep disk use in check by itself. Pick the two limits from their pop-up menus:

| Setting | Default | What it does |
| --- | --- | --- |
| **Remove a project's build cache after** | 30 days | A project you have not opened for this long loses its build cache. |
| **Keep all build caches under** | 10 GB | When all build caches together are bigger, Studio removes those of the projects you opened least recently, until they fit. |

When it is on, it runs a few seconds after Studio starts, then once a day while Studio stays open. It also removes the temp files of projects other than the open one.

- **The open project is never cleaned automatically.**
- Each removal is reported in the **Output** tab, for example: `[storage] freed 3.9 GB: build cache of 'Space Game', not opened for 41 days`.

Settings are saved for your user account, not per project.
