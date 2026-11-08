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
| Starter templates | Blank, 2D starter and 3D starter projects | Gives a working example scene to learn from instead of an empty screen | ✅ |
| Safe project upgrades | Older projects are upgraded automatically, with a backup of the old file | Opening an old project never loses its settings | ✅ |
| Unsaved changes | ⌘S (or File → Save Scene) saves. Studio asks you to save only when your game really differs from what's on disk: the scene, project settings or a code file. Undoing back to the saved state counts as saved. Editor layout, open panels and where the Scene View is looking are saved automatically and never count. Discard throws the changes away for good | Never lose work, and never get asked about changes you didn't make | ✅ |
| Storage | Settings (⌘,) shows what each project keeps on disk (build caches, temp files, old engine copies, backups) with Clean buttons; optional automatic cleanup (off by default; 30 days unused, 10 GB cap). Never deletes project files, asset IDs or saves | Stop build caches from filling the disk over time | ✅ |

## 1b. Scene View (building the world)

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Camera navigation | Right-drag, ⌘+drag or ⌘+two-finger scroll to orbit; two-finger trackpad scroll, middle-drag or Shift+right-drag to pan; mouse wheel or pinch to zoom toward the cursor; hold right mouse + WASD/QE to fly; double-click a spot to glide there | Move freely around the world while building it, with a mouse or a trackpad | ✅ |
| Navigation tools | Two tools, as in Figma: Select (V, default) and Hand (H, or hold Space) choose what a plain left-drag does; ⌘ + drag rotates and pinch/wheel zoom with either tool | Move around with one mouse button or trackpad finger | ✅ |
| Frame selected | Press F over the Scene View, or double-click an object in the Hierarchy, to fly the camera to it (F2 renames in the Hierarchy) | Find and focus on an object quickly | ✅ |
| Transform gizmo | One combined gizmo, no modes: arrows move along an axis, a knob on each arrow scales along that axis, faint rotation rings turn like a dial, and the centre square scales evenly; drawn on top at a constant size, hovered handle brightens, dragged handle turns yellow | Move, rotate and resize objects without switching tools | ✅ |
| Object icons | Cameras, point lights and sound sources show a small icon (camera, bulb, speaker) at a fixed size; click an icon to select its object (Cmd-click adds). A red box marks a model that failed to load | Find and pick objects that have no visible shape | ✅ |
| Click to select | Click any model, sprite or icon in the Scene View to select it (exact triangle hit, nearest wins); Cmd-click adds or removes; click empty space to deselect. The Hierarchy scrolls to the row and opens its parents | Pick objects directly in the world | ✅ (instanced foliage groups not yet clickable) |
| Delete objects | Select objects and press Backspace or Delete (or right-click → Delete); their children go too, and ⌘Z brings them back | Remove things quickly while building | ✅ |
| Groups | Select objects and press ⌘G to put them in an empty "Group" object, shown as a folder in the Hierarchy; ⇧⌘G ungroups. Nothing moves on screen, and it can be undone | Keep a long Hierarchy tidy (all trees in "Trees") and move related objects together | ✅ |
| Drag into the scene | Drag a file from Project Assets over the Scene View: a marker shows where it will land, on the object under the cursor, on the ground or in front of the camera. Drop it on a Hierarchy row to make it that object's child | Place things where you want them instead of finding them at the world's centre | ✅ |
| Duplicate, copy and paste | ⌘D copies the selected objects next to the originals; ⌘C and ⌘V copy and paste them, also between scenes of a project. Everything an object has comes along, including children | Build levels quickly from what is already there | ✅ |
| Move several objects | Drag a selected row to move the whole selection: into another object, between rows or to the scene root. They stay where they are in the world | Tidy up a long Hierarchy in one go | ✅ |
| Hide in the editor | The eye on a Hierarchy row hides an object and its children in the Scene View only. The game is not affected and the scene does not change. Hidden lights still shine | See and click what is behind something while you work | ✅ |
| Create menu | Right-click a Hierarchy row to add an empty object, camera, point light, sound, sprite, UI label or UI button inside it; right-click below the rows, or use +, to add one at the top level | Add objects where you want them | ✅ |
| Hierarchy icons | Each Hierarchy row has an icon for what the object is: camera, light, sound, on-screen element, tilemap, water, sprite, terrain, model or group. Cameras, lights and sounds use the same shapes as the Scene View | Find things in a long list at a glance | ✅ |
| Lock objects | Locked objects cannot be clicked or moved in the Scene View (no gizmo), but can still be selected in the Hierarchy and used as a surface for double-click focus | Protect finished work, such as terrain, from accidental edits | ✅ |
| Selection highlight | An orange box hugs the selected object (and its children) and its colliders are drawn in orange; other colliders appear only with Overlays → All colliders (off by default, so the scene looks like the game) | See clearly what is selected | ✅ |

## 2. Writing gameplay in Lua

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Lua scripts | Small script files attached to objects in the scene | Making objects move, react to keys, collide, play sounds, show, hide or remove objects, pause the game, change scenes and keep score | 🟡 no skeleton animation control yet |
| Math and input in Lua | Scripts can work with 3D positions as vectors, turn, scale and aim objects, and read every key, mouse button and gamepad control, including locking the mouse for look-around (scaling an object does not resize its collider yet) | Spinning objects, first-person style movement, aiming at targets and gamepad support | ✅ |
| Physics in Lua | Scripts can push things, move characters that slide along walls, shoot rays to see what is ahead, and react to triggers such as coin pickups. Each collider has settings in the Inspector: trigger, mass (or Auto), friction, bounciness, gravity and keep upright | Jumping and shoving crates, platformer and top-down characters, ground checks, pickups and goal zones | ✅ |
| Safe sandbox | Scripts cannot touch files, the network or the computer, and are stopped if they run too long or use too much memory | A broken or endless script shows an error instead of freezing the game | ✅ |
| Code editor help | Lua colouring, autocomplete, hover help and error marks before Play | Beginners see mistakes and available commands while typing | ✅ |
| Save data | Store small values such as score or level between play sessions | Keep progress after the game is closed | ✅ |
| Scripted prefabs | Spawn a saved object during play with its own script already attached and started | Enemies, pickups and bullets that each run their own behaviour | ✅ |

## 3. Writing gameplay in Rust

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Rust starter game | A ready-to-compile Rust game that loads your scene | Starting point for developers who prefer Rust | ✅ |
| Bundled engine copy (SDK) | Each Rust project gets its own copy of the engine source, upgradeable from the File menu | The project builds anywhere, without needing this repository | ✅ |
| Same features as Lua | Rust games use the same engine gameplay runtime as Lua: moving and spawning objects, prefabs, sounds, collisions, visual rules, variables and scene changes. Rust projects created before visual rules were added must call `start_rules` (after `setup`) and `update_rules` (every `update`) in their `src/main.rs`, or their scenes' rules will not run. Your own Rust `update` keeps running while the game is paused, so check for the pause yourself | Rust games can do everything Lua games can, and new starters show where to add gameplay | ✅ |

## 4. Play and export

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Run button | Runs the game in its own window, exactly as players get it. Lua games start at once; Rust games first compile only the game (not Studio), reusing earlier builds so a re-run takes seconds. Build progress and game messages appear in the Output tab, errors also in Problems | See the real game and where it broke | ✅ |
| Play in the Scene View | ▶ Play, ⏸ Pause and ⏭ Step in the Scene View header (⌘P, ⇧⌘P, ⌥⌘P): the same game program as Run, shown inside the Scene View with keyboard and mouse passed through. Works for Lua and Rust projects; the scene is read-only while playing | Quickly see what the world looks like in the game without leaving the editor, and freeze a moment to look at it | 🟡 no free camera while paused |
| Export | Packs the game into a folder that runs on its own | Share the finished game with players | 🟡 tested on macOS only |
| Studio package | Bundles Studio with everything it needs (player, code editor, Lua helper) | Install Studio on a machine without Rust | 🟡 macOS only, not a signed installer |

## 5. Building 3D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Terrain | Create ground and sculpt hills and valleys with a brush; every stroke can be undone and redone | Landscapes for 3D games | 🟡 no texture painting or large worlds yet |
| Foliage | Scatter trees or rocks over terrain | Fill a landscape quickly | 🟡 must be re-scattered after sculpting |
| Water | Add a water surface with waves and currents | Lakes, seas and swimming | ✅ |
| Lighting | Sun, ambient light and point lights; with many lights, the 16 that matter most to the camera are used | Set the mood and visibility of a scene | ✅ |
| Cameras | A camera shows the world from its object; mark one as active and the game looks through it. A Camera Follow section makes it trail an object: behind a character, from its eyes (first person, turned by the mouse or stick) or along a 2D level. A key can flip it between first and third person while playing. Scripts can switch cameras and shake the screen | Set up what the player sees | ✅ |
| Environment | Scene-wide settings with no position, in the Scene View's Environment menu: ambient sounds (wind, music) that loop at the same volume everywhere, plus sun and ambient light | Effects that belong to the whole scene, not to one object | ✅ |
| Materials | Change a model's colour, shininess and roughness | Make imported models look different without editing them | 🟡 no textures or shader graph yet |

## 6. Building 2D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Sprites | Drag an image into the scene to make an object | Characters, items and backgrounds | ✅ |
| Sprite sheet animation | Play frames from a sprite sheet as a flipbook | Walking, idle and other simple animations | ✅ |
| Tilemaps | Paint levels from a tile sheet, optionally solid | Platformer and top-down levels | 🟡 no layers or auto-tiling |
| 2D collision | Box and circle colliders fitted to sprites | Walls, floors, balls and pickups | ✅ |

## 7. Game UI

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Labels and buttons | On-screen text and clickable buttons placed in the editor | Score displays, menus, start buttons | 🟡 no images or text input yet |

## 8. Gameplay without code

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Visual rules | "When this happens, do that" rules on an object. Triggers: button clicked, named event, scene starts, key pressed, touching or leaving another object or a trigger zone, a timer (once or repeating), a variable changed, the game state changed. Actions: change text, play a sound, send an event, set or add to a variable, change scene, show or hide, enable or disable, spawn a saved object, destroy, set the game state | Simple game logic without writing a script | 🟡 rules on objects spawned while playing do not run; scenes using the newer triggers and actions need the current Rust SDK in Rust projects |
| Variables and conditions | Rules can set or add to a number (e.g. `coins`) and run only if up to 8 checks pass, either all of them or any of them (e.g. `coins ≥ 3`); text can show a value with `{coins}` | Scores, counters and win conditions without code; Lua can read and write the same values | ✅ |
| Game state | The game is always in one of four states: menu, playing, paused or game over (it starts as playing). Pausing freezes scripts' update, physics, timers, animations and camera follow and shake. Buttons, events, key-press rules, state-changed rules and sounds already playing keep going, so a menu can unpause. The state carries over to the next scene. Choose a scene's start state in the Scene View's Environment menu. Lua can read and change it, and gets a call when it changes | Pause menus, title screens and game-over screens | ✅ |
| Show, hide and remove | Objects can be hidden, disabled or destroyed by rules or Lua. A disabled object is not drawn in the game, has no physics, and its scripts and rules do nothing (Studio's Scene View still shows it). An object that starts disabled works fully once it is enabled: its rules run, its on-screen text or button appears, and its script starts. A disabled object also disables its children in the game. Showing or enabling a parent also does it for children that were hidden on their own. Timers of a disabled object keep counting, so a once-only timer that runs out while it is disabled is used up | Doors, pickups, enemies and pop-up screens | ✅ |
| Gameplay graph | A canvas of event and action nodes joined by curved lines; drag nodes to arrange them (layout saves with the scene), drag empty space to pan, click an event then an action to connect them | See and rewire game logic visually | ✅ |
| Blueprint-style graph | A full visual programming graph | Bigger games without code | ⬜ |
| Shader graph | Build visual effects from nodes | Custom looks without shader code | ⬜ |

## 9. Still to do

- Test the 2D and 3D starter projects in the running editor.
- Package and test Studio on Windows and Linux, including a clean machine without Rust.
- Blueprint-style graph and shader graph.
