# RustyEngine Studio — Scene View Guide

The Scene View is where you build your game world: you place, select, move and arrange objects, sculpt terrain, paint tiles, and set scene-wide environment settings. It is an editing view. To see the game as a player will, press **Run**, which launches the game in its own window.

For the camera and selection controls, see [Keyboard and Mouse Shortcuts](01_KEYBOARD_SHORTCUTS.md).

---

## Layout

| Area | Location | Contents |
| --- | --- | --- |
| Header | Top of the panel | Centre: **Play**, **Pause** and **Step**. Right: navigation tools **Select** and **Hand**. Hover a button to see its name. |
| Viewport | Centre | The 3D or 2D view of the scene |
| Footer | Bottom of the panel | **3D / 2D**, **Global / Local**, **Overlays**, **Terrain**, **Tilemap**, **Environment** |

---

## What you see in the viewport

| Marker | Meaning |
| --- | --- |
| Yellow box | The selected object, fitted to its model. Selecting a parent also boxes its children. |
| Arrows, rings and a centre square | The transform gizmo for the selected object (see below): red = X, green = Y, blue = Z. |
| Round icon (camera, bulb, speaker) | An object that has a position but no visible shape: a camera, a point light, or a sound source. Click the icon to select the object. A yellow ring marks the selected icon. |
| Red box | An object whose model is set but could not be loaded. Check that the file still exists in Project Assets. |

Objects with no shape and no special role (empty objects used to group others) show no marker. Select them in the Hierarchy.

---

## Navigation tools (header)

Two tools decide what a plain left-click or left-drag does. The highlighted button is the active tool.

| Tool | Key | Left-click | Left-drag | Pointer |
| --- | --- | --- | --- | --- |
| **Select** (default) | V | Selects the object under the pointer | Drags the selected object's gizmo handles | Arrow |
| **Hand** | H, or hold **Space** | Nothing | Moves the view; ⌘ + drag rotates it | Open hand |

**Hold Space** to switch to Hand only while the key is down. Release it to return to Select. This is the quickest way to move around while editing.

While Hand is active, gizmo handles are hidden and clicks do not change the selection. With either tool, the other camera controls keep working: ⌘ + drag or right-drag to rotate, pinch or the scroll wheel to zoom, double-click to glide, and **F** to frame the selection.

---

## Transform gizmo

Select an object to show its gizmo. One gizmo moves, rotates and scales, so there is no mode to switch.

| Handle | Looks like | Drag to |
| --- | --- | --- |
| **Arrows** | Red (X), green (Y) and blue (Z) arrows out of the object | Move the object along that axis. The object follows the pointer. |
| **Scale knobs** | A small square on each arrow, in the arrow's colour | Stretch or squash the object along that axis only: drag toward the arrow tip to grow, toward the centre to shrink. |
| **Rotation rings** | A thin arc around the object for each axis, in the same colours. They appear when you move the pointer near the object, and only the front half of each ring is drawn. | Turn the object around that axis, like a dial. Over a ring the pointer changes to a curved double arrow. |
| **Centre square** | A small white square at the object's centre | Scale the whole object evenly: drag right or up to grow, left or down to shrink. |

Away from the object you see only the arrows with their scale knobs and the centre square, so the gizmo stays uncluttered. Move the pointer toward the object and the rings appear; they stay faint until you hover one. The handle under the pointer brightens, and the handle you are dragging turns yellow. The gizmo is drawn on top of the scene and stays the same size on screen at any zoom.

In the 2D view the gizmo shows the X and Y arrows (with their scale knobs) and a single ring for turning in the screen plane.

A scale knob stretches the object along its own axis that lines up best with the knob's arrow. For an object that has not been rotated, that is exactly the arrow's axis. For a rotated object, check the result, or type exact values in the object's **Scale** fields in the Inspector.

Every selected object that is not locked changes together. A locked object shows no gizmo.

---

## Locked objects

Lock an object to protect it from accidental changes while you work around it, for example the terrain while you place trees on it.

| | Unlocked | Locked |
| --- | --- | --- |
| Click it in the Scene View | Selects it | Ignored; the click goes to whatever is behind it, or clears the selection |
| Its icon (camera, light, sound) | Shown and clickable | Hidden |
| Gizmo handles | Shown | Hidden; it cannot be moved, rotated or scaled with the gizmo |
| Double-click it to glide the camera there | Works | Works; a locked terrain is still a surface you can fly to |
| Select it in the Hierarchy | Works | Works, so you can still edit it in the Inspector or unlock it |

To lock or unlock an object, right-click it in the Hierarchy and choose **Lock** or **Unlock**, or tick **Locked** in the Inspector. A lock icon appears next to locked objects in the Hierarchy. The lock is saved with the scene.

---

## 3D / 2D

| Option | What happens |
| --- | --- |
| **3D** | Perspective view. You can orbit, pan, zoom and fly. |
| **2D** | Flat front view for sprites and tilemaps. You can pan and zoom; rotation is off. Tile painting is available only here. |

Each mode keeps its own camera position.

---

## Global / Local

These two buttons in the footer are meant to switch the gizmo between world axes (**Global**) and the selected object's own axes (**Local**).

> **Current limitation:** only **Global** works today. The gizmo always uses world axes, whichever option is chosen.

---

## Overlays

Overlays are visual aids for building the scene. They are never shown in the running game. Each one turns on or off independently, and your choices are remembered per project.

| Option | When on | Typical use |
| --- | --- | --- |
| **All colliders** | Draws every object's physics shape as a green wireframe, with the selected object's in yellow. | Checking where the player can walk or collide |
| **Lights** | Draws the sun's direction as an arrow and a marker at each point light. | Aiming the sun and placing lamps |
| **Cameras** | Draws each camera's view volume. | Seeing what a game camera will show |
| **Ground grid** | Draws a flat 40 × 40 m grid of 1 m squares at height 0, centred on the world origin. | Lining objects up and judging distances |
| **Audio emitters** | Draws a marker at each sound source and a ring at the distance where it fades to silence. | Placing sounds and checking their reach |
| **Nav grid** | Draws the scene's navigation grid: walkable cells outlined, blocked cells crossed out in red. | Checking where characters can find a path |

With every overlay off, the scene looks the way it will in the game, apart from the selection markers.

---

## Terrain

| Option | What happens |
| --- | --- |
| **Create terrain** | Adds a flat terrain to the scene and saves it as a terrain file plus a model in Project Assets. |
| **Create water** | Adds a water surface. Edit its size, waves and current in the Inspector. |
| **Sculpt in Scene View** | Turns the left mouse button into a sculpting brush on the selected terrain. Each stroke is saved when you release the button and can be undone with ⌘Z. While on, left clicks do not select objects. |
| **Brush** | **Raise** builds up, **Lower** digs down, **Smooth** evens out bumps, **Flatten** levels toward a fixed height. |
| **Radius** | Size of the brush (0.01–100 m). |
| **Strength** | How strongly each stroke changes the ground. |
| **Density**, **Spacing**, **Seed** | Settings for foliage scatter (below). The same seed always gives the same layout. |
| **Scatter selected model on terrain** | Places copies of a model across the terrain. Select the terrain in the Hierarchy and a model in Project Assets first. |

Scattered foliage is placed once. If you sculpt the terrain afterwards, delete the foliage group and scatter it again.

---

## Tilemap

Available in the **2D** view.

| Option | What happens |
| --- | --- |
| **Create from selected image** | Creates a tilemap that uses the image selected in Project Assets as its tile sheet. Set the grid and tile size in the Inspector. |
| **Paint in Scene View** | Click or drag on the selected tilemap to paint the brush tile chosen in the Inspector. While on, left clicks do not select objects. |
| **Erase cells** | Painting removes tiles instead of adding them. |

Turn on **Paint solid tiles** in the tilemap's Inspector to make painted tiles block movement. Tile painting can be undone with ⌘Z.

---

## Playing in the Scene View

The three buttons in the middle of the header play your game inside the Scene View, for a quick look while you build the world.

| Button | Shortcut | What it does |
| --- | --- | --- |
| ▶ **Play** (■ **Stop** while playing) | ⌘P | Starts the game in the Scene View, or stops it |
| ⏸ **Pause** | ⇧⌘P | Freezes the game: no time passes, nothing moves, sound is muted. Click again to resume. |
| ⏭ **Step** | ⌥⌘P | While paused, advances the game by exactly one frame |

**What happens when you press Play:**

1. Unsaved changes to the scene are saved, as with **Run**.
2. A Rust game is checked first, always in the Debug configuration (the fastest to compile). If you only changed the world (objects, lights, terrain), nothing needs compiling: the Scene View says "Starting the game…" and the game appears in about a second. Your world is read from the scene file when the game starts, so it never needs compiling. Only when Rust code changed does it say "Building the game…", compiling just the game's own code, with progress in the **Output** tab. A Lua game never compiles.
3. The game appears in the Scene View with a thin frame around it (blue while the game has the keyboard), at the same shape as its picture (bars fill any leftover space).

**Playing:**

- **Click the game** to give it the keyboard and mouse. The frame turns brighter. Click any other panel to take the keyboard back.
- While paused, a **Paused** badge shows at the top of the game.
- The **Hierarchy** and **Inspector** are read-only while playing. The game runs from the saved scene file, so edits made now could not reach it.
- Game messages appear in **Output**, and Lua errors in **Problems**, the same as with Run.
- If the game stops with an error, a red banner across the Scene View says why. It disappears the next time you press Play.

**Play compared with Run:**

| | Play | Run |
| --- | --- | --- |
| Where | Inside the Scene View | The game's own window |
| The game | The same program as Run | The same program |
| Build | Always Debug | The selected build configuration |
| Picture size | The Scene View's size (up to 1920×1080) | The game's own window size |
| Pause and Step | Yes | No |

Use Play for quick checks while you work, and Run to see exactly what players get, including full speed in a Release build.

**Limits:**

- Rust projects created with an older Studio need **File → Upgrade Rust SDK** before they can play in the Scene View. Studio says so if a game does not start in preview mode. **Run** works either way.
- While paused you see the frozen picture only. You cannot move the editor camera around a paused game yet.
- Only one game runs at a time: Play and Run cannot both be active.

---

## Environment

Scene-wide settings that have no position in the world.

| Option | What happens |
| --- | --- |
| **Ambient sounds** | Sounds that loop at the same volume everywhere in the scene, such as wind or background music. Each has a **Volume** (0–4) and a **Remove** button. Up to 8 per scene. |
| **Add selected sound** | Adds the sound file selected in Project Assets (WAV) as an ambient sound. |
| **Create point light** | Adds a light at a position. Move it with the gizmo; set its color, strength and range in the Inspector. Up to 16 point lights nearest the camera light the scene at once. |
| **Override scene lighting** | Gives the scene its own sun and ambient light instead of the default daylight. |
| **Sun direction**, **Sun strength**, **Sun color** | Where the sunlight comes from, how bright it is, and its color. |
| **Ambient** | How bright the shaded areas are. |

A sound that belongs to an object (footsteps, an engine, a waterfall) is not an ambient sound. Add an **Audio Emitter** component to that object instead (Inspector → **Add Component**), so the sound comes from the object and fades with distance.

Ambient sounds play when the game runs (Play, Run or an exported game). They do not play while editing.

