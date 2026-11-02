# RustyEngine Studio Features

What Studio can do for game makers, in plain language, and how far each feature has come.

**Goal:** a beginner can use RustyEngine Studio to create a 2D or 3D game, play it, find and fix mistakes, and export it as a finished game.

- **Gameplay code:** you write it in **Lua** (recommended for beginners) or **Rust**, chosen when the project is created.
- **The engine:** both kinds of project share the same scenes, graphics, physics, sound and assets.
- **Implementation plans:** kept separately under `.superpowers/` (specs and plans).
- **Engine runtime inventory:** [02_ENGINE_FEATURES.md](02_ENGINE_FEATURES.md).
- **How to use the editor (user docs):** [Keyboard and Mouse Shortcuts](../user/01_KEYBOARD_SHORTCUTS.md) and the [Scene View Guide](../user/02_SCENE_VIEW_GUIDE.md).

Status: ✅ done · 🟡 partly done · ⬜ not started

---

## 1. Projects

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Lua or Rust project | Each new project picks one gameplay language | Lets beginners use easy Lua, while experienced developers can still use Rust | ✅ |
| Starter templates | Blank, 2D starter and 3D starter projects | Gives a working example scene to learn from instead of an empty screen | 🟡 not yet checked in a running editor |
| Safe project upgrades | Older projects are upgraded automatically, with a backup of the old file | Opening an old project never loses its settings | ✅ |
| Storage | Settings (⌘,) shows what each project keeps on disk (build caches, temp files, old engine copies, backups) with Clean buttons; optional automatic cleanup (off by default; 30 days unused, 10 GB cap). Never deletes project files, asset IDs or saves | Stop build caches from filling the disk over time | 🟡 not yet checked in a running editor |

## 1b. Scene View (building the world)

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Camera navigation | Right-drag, ⌘+drag or ⌘+two-finger scroll to orbit; two-finger trackpad scroll, middle-drag or Shift+right-drag to pan; mouse wheel or pinch to zoom toward the cursor; hold right mouse + WASD/QE to fly; double-click a spot to glide there | Move freely around the world while building it, with a mouse or a trackpad | ✅ |
| Navigation tools | Two tools, as in Figma: Select (V, default) and Hand (H, or hold Space) choose what a plain left-drag does; ⌘ + drag rotates and pinch/wheel zoom with either tool | Move around with one mouse button or trackpad finger | ✅ |
| Frame selected | Press F over the Scene View, or double-click an object in the Hierarchy, to fly the camera to it (F2 renames in the Hierarchy) | Find and focus on an object quickly | ✅ |
| Transform gizmo | One combined gizmo, no modes: arrows move along an axis, a knob on each arrow scales along that axis, faint rotation rings turn like a dial, and the centre square scales evenly; drawn on top at a constant size, hovered handle brightens, dragged handle turns yellow | Move, rotate and resize objects without switching tools | ✅ |
| Object icons | Cameras, point lights and sound sources show a small icon (camera, bulb, speaker) at a fixed size; click an icon to select its object (Cmd-click adds). A red box marks a model that failed to load | Find and pick objects that have no visible shape | ✅ |
| Click to select | Click any model, sprite or icon in the Scene View to select it (exact triangle hit, nearest wins); Cmd-click adds or removes; click empty space to deselect. The Hierarchy scrolls to the row and opens its parents | Pick objects directly in the world | ✅ (instanced foliage groups not yet clickable) |
| Groups | Select objects and press ⌘G to put them in an empty "Group" object, shown as a folder in the Hierarchy; ⇧⌘G ungroups. Nothing moves on screen, and it can be undone | Keep a long Hierarchy tidy (all trees in "Trees") and move related objects together | ✅ |
| Lock objects | Locked objects cannot be clicked or moved in the Scene View (no gizmo), but can still be selected in the Hierarchy and used as a surface for double-click focus | Protect finished work, such as terrain, from accidental edits | ✅ |
| Selection highlight | An orange box hugs the selected object (and its children) and its colliders are drawn in orange; other colliders appear only with Overlays → All colliders (off by default, so the scene looks like the game) | See clearly what is selected | ✅ |

## 2. Writing gameplay in Lua

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Lua scripts | Small script files attached to objects in the scene | Making objects move, react to keys, collide, play sounds, change scenes and keep score | 🟡 no skeleton animation control yet |
| Safe sandbox | Scripts cannot touch files, the network or the computer, and are stopped if they run too long or use too much memory | A broken or endless script shows an error instead of freezing the game | ✅ |
| Code editor help | Lua colouring, autocomplete, hover help and error marks before Play | Beginners see mistakes and available commands while typing | ✅ |
| Save data | Store small values such as score or level between play sessions | Keep progress after the game is closed | ✅ |
| Scripted prefabs | Spawn a saved object during play with its own script already attached and started | Enemies, pickups and bullets that each run their own behaviour | ✅ |

## 3. Writing gameplay in Rust

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Rust starter game | A ready-to-compile Rust game that loads your scene | Starting point for developers who prefer Rust | ✅ |
| Bundled engine copy (SDK) | Each Rust project gets its own copy of the engine source, upgradeable from the File menu | The project builds anywhere, without needing this repository | ✅ |
| Same features as Lua | Rust games use the same engine gameplay runtime as Lua: moving and spawning objects, prefabs, sounds, collisions, visual rules, variables and scene changes | Rust games can do everything Lua games can, and new starters show where to add gameplay | ✅ |

## 4. Play and export

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Run button | Runs the game in its own window, exactly as players get it. Lua games start at once; Rust games first compile only the game (not Studio), reusing earlier builds so a re-run takes seconds. Build progress and game messages appear in the Output tab, errors also in Problems | See the real game and where it broke | ✅ |
| Play in the Scene View | ▶ Play, ⏸ Pause and ⏭ Step in the Scene View header (⌘P, ⇧⌘P, ⌥⌘P): the same game program as Run, shown inside the Scene View with keyboard and mouse passed through. Works for Lua and Rust projects; the scene is read-only while playing | Quickly see what the world looks like in the game without leaving the editor, and freeze a moment to look at it | 🟡 not yet checked in a running editor; no free camera while paused |
| Export | Packs the game into a folder that runs on its own | Share the finished game with players | 🟡 tested on macOS only |
| Studio package | Bundles Studio with everything it needs (player, code editor, Lua helper) | Install Studio on a machine without Rust | 🟡 macOS only, not a signed installer |

## 5. Building 3D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Terrain | Create ground and sculpt hills and valleys with a brush; every stroke can be undone and redone | Landscapes for 3D games | 🟡 no texture painting or large worlds yet |
| Foliage | Scatter trees or rocks over terrain | Fill a landscape quickly | 🟡 must be re-scattered after sculpting |
| Water | Add a water surface with waves and currents | Lakes, seas and swimming | ✅ |
| Lighting | Sun, ambient light and point lights; with many lights, the 16 that matter most to the camera are used | Set the mood and visibility of a scene | ✅ |
| Environment | Scene-wide settings with no position, in the Scene View's Environment menu: ambient sounds (wind, music) that loop at the same volume everywhere, plus sun and ambient light | Effects that belong to the whole scene, not to one object | ✅ |
| Materials | Change a model's colour, shininess and roughness | Make imported models look different without editing them | 🟡 no textures or shader graph yet |

## 6. Building 2D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Sprites | Drag an image into the scene to make an object | Characters, items and backgrounds | ✅ |
| Sprite sheet animation | Play frames from a sprite sheet as a flipbook | Walking, idle and other simple animations | ✅ |
| Tilemaps | Paint levels from a tile sheet, optionally solid | Platformer and top-down levels | 🟡 no layers or auto-tiling |
| 2D collision | Box colliders fitted to sprites | Walls, floors and pickups | 🟡 boxes only |

## 7. Game UI

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Labels and buttons | On-screen text and clickable buttons placed in the editor | Score displays, menus, start buttons | 🟡 no images or text input yet |

## 8. Gameplay without code

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Visual rules | "When this happens, do that" rules on an object (change text, play a sound, send an event) | Simple game logic without writing a script | ✅ |
| Variables and conditions | Rules can set or add to a number (e.g. `coins`) and run only if a check passes (e.g. `coins ≥ 3`); text can show a value with `{coins}` | Scores, counters and win conditions without code; Lua can read and write the same values | ✅ |
| Gameplay graph | A canvas of event and action nodes joined by curved lines; drag nodes to arrange them (layout saves with the scene), drag empty space to pan, click an event then an action to connect them | See and rewire game logic visually | ✅ |
| Blueprint-style graph | A full visual programming graph | Bigger games without code | ⬜ |
| Shader graph | Build visual effects from nodes | Custom looks without shader code | ⬜ |

## 9. Still to do

- Test the 2D and 3D starter projects in the running editor.
- Package and test Studio on Windows and Linux, including a clean machine without Rust.
- Blueprint-style graph and shader graph.
