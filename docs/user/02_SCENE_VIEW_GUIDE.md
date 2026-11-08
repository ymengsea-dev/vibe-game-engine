# Scene View Guide

The Scene View is where you build your game world. For keys and mouse moves, see [Shortcuts](01_KEYBOARD_SHORTCUTS.md).

## The panel

| Part | What is there |
| --- | --- |
| Top middle | **Play**, **Pause**, **Step** |
| Top right | **Select** and **Hand** tools |
| Bottom | **3D / 2D**, **Global / Local**, **Overlays**, **Terrain**, **Tilemap**, **Environment** |

## What you see

| Mark | Meaning |
| --- | --- |
| Yellow box | The selected object |
| Arrows, rings, small squares | Handles to move, turn and stretch the selected object (red = X, green = Y, blue = Z) |
| Round icon (camera, bulb, speaker) | An object you can't see in the game. Click the icon to select it. |
| Red box | A model file that could not be loaded |

Empty objects (like groups) show nothing. Select them in the Hierarchy.

## Tools

| Tool | What a left-drag does |
| --- | --- |
| **Select** (V) | Clicks select, drags move the handles |
| **Hand** (H, or hold Space) | Moves the view |

## Handles

| Handle | Drag it to |
| --- | --- |
| Arrow | Move along that line |
| Ring (shows when the mouse is near the object) | Turn around that line |
| Small square on an arrow | Stretch along that line |
| White square in the centre | Make the whole object bigger or smaller |

In 2D you get two arrows and one ring.

## Locked objects

When you drag a file from Project Assets over the Scene View, a ring with the file's name shows where it will land: on the object under the cursor (locked ones count, so a locked terrain works), else on the ground, else in front of the camera. In the 2D view it lands at the cursor. The object's own centre goes on that point, so a model whose centre is in its middle sits partly in the ground until you move it up. If a prefab can't be loaded, the Problems tab says why.

The eye at the end of a Hierarchy row hides an object in the Scene View: it isn't drawn and can't be clicked, and its children go with it. This only affects the editor. It isn't saved in the scene, doesn't count as a change, and the game still shows the object. A hidden point light still lights the scene. Studio remembers what you hid the next time you open the project.

A locked object can't be clicked or moved in the Scene View, so you don't change it by mistake. You can still select and edit it in the Hierarchy and Inspector. Lock it with right-click → **Lock**, or the **Locked** box in the Inspector.

## Disabled objects

A disabled object is still drawn in the Scene View, but in the game it is hidden, has no physics, and its scripts and rules wait. A rule or script can turn it back on. Disabling a parent also disables everything inside it.

## Bottom bar

| Button | What it does |
| --- | --- |
| **3D** | Normal 3D view |
| **2D** | Flat view for sprites and tiles. Tile painting works only here. |
| **Global / Local** | Only **Global** works for now |

3D and 2D each remember where you were looking. Studio also remembers your view for each project.

## Overlays

Helper drawings. The game never shows them.

| Overlay | Shows |
| --- | --- |
| **All colliders** | Physics shapes: green, the selected one yellow, triggers purple |
| **Lights** | Each lamp and how far its light reaches |
| **Cameras** | What each game camera sees |
| **Ground grid** | 1 m squares on the floor |
| **Audio emitters** | Each sound and how far it can be heard |
| **Nav grid** | Where characters can walk (red = blocked) |

## Cameras

A camera looks the way its object faces. Move and turn the object to aim it.

| Setting | What it does |
| --- | --- |
| **Projection** | **Perspective** (normal 3D) or **Orthographic** (flat, for 2D) |
| **Field of view** | How wide the camera sees (perspective) |
| **Size** | How much fits on screen (orthographic) |
| **Near**, **Far** | Closest and farthest things the camera draws |
| **Active camera** | The camera the game uses. Only one. If none is ticked, the first camera is used. |

**Camera Follow** (Add component, cameras only) makes the camera follow a **Target**:

| Mode | Settings |
| --- | --- |
| **Third person** | **Distance** and **Height** behind the target, **Smoothing**, **Orbit with** (mouse or stick), **Pitch limits** |
| **First person** | **Height** of the eye above the target's origin, **Look with** (mouse or stick), **Pitch limits**. The camera does not turn or move the target: walking still needs a Lua or Rust script, which can walk the way the camera faces |
| **2D side** | **Dead zone** (how far the target moves before the camera does), **Limit to area**, **Smoothing** |

**Switch view with a key** (first and third person only) flips the camera between the two while playing, like pressing V in many games, still looking the same way. Each view keeps its own settings: choose the other **Mode** to edit them.

When the mouse turns a first- or third-person camera, the game locks the mouse by itself while it is being played and frees it when paused or in a menu. Buttons on screen can't be clicked while it is locked, so pause first, or free it from a script.

Smoothing: higher stays closer, lower lags behind, 0 follows exactly.

Limits: a following camera can't be inside another object. Things moved by animation show one frame late.

## Terrain

| Option | What it does |
| --- | --- |
| **Create terrain** | Adds flat ground |
| **Create water** | Adds water. Change waves in the Inspector. |
| **Sculpt in Scene View** | Drag on the ground to shape it with the **Brush**: **Raise**, **Lower**, **Smooth**, **Flatten**. Set **Radius** and **Strength**. |
| **Scatter selected model** | Spreads copies of a model (trees, rocks) over the ground. **Density**, **Spacing**, **Seed** control how. |

If you reshape the ground after scattering, delete the copies and scatter again.

## Tilemap (2D view)

| Option | What it does |
| --- | --- |
| **Create from selected image** | Makes a tile map from an image in Project Assets |
| **Paint in Scene View** | Click or drag to paint tiles |
| **Erase cells** | Painting removes tiles |

Tick **Paint solid tiles** in the Inspector to make tiles block movement.

## Playing in the Scene View

| Button | Key | What it does |
| --- | --- | --- |
| **Play / Stop** | ⌘P | Run the game right here |
| **Pause** | ⇧⌘P | Freeze the game |
| **Step** | ⌥⌘P | When paused, move one frame ahead |

- Play saves your scene first. Lua games start at once; Rust games build first if the code changed.
- Click the game to control it. Click another panel to get the keyboard back.
- If the game holds the mouse, press **Esc** to free it. Esc does not stop the game. Click the game to lock the mouse again; that click does not count as a click in the game.
- You can't edit the scene while playing.
- Errors show in **Output** and **Problems**.

**Play or Run?** Play is a quick check inside Studio. **Run** opens the game in its own window, the way players see it.

Limits: older Rust projects need **File → Upgrade Rust SDK** to play here. Play and Run can't both be on.

## Environment

Settings for the whole scene.

| Option | What it does |
| --- | --- |
| **Ambient sounds** | Sounds heard everywhere, like wind or music (up to 8) |
| **Add selected sound** | Adds the WAV file picked in Project Assets |
| **Create point light** | Adds a lamp |
| **Override scene lighting** | Set your own sun: **Sun direction**, **Sun strength**, **Sun color**, **Ambient** (shadow brightness) |
| **Start state** | How the game starts: **Playing**, **Menu**, **Paused** or **Game over**. Only the first scene's choice counts. |

A sound that comes from one thing (footsteps, a waterfall) goes on that object instead: Add component → **Audio Emitter**.
