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
| Flat shapes and text fixed to the view | The scene's Game UI, placed as in the game. Blue outline: the selected element. Bars and sliders are drawn 60% full, because no game is running to fill them. Text uses Studio's font here and Inter when playing, so its width can differ a little |

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

A locked object can't be changed until you unlock it. It can't be clicked or moved in the Scene View, painted or sculpted, renamed, or deleted, and the Inspector shows it read-only with only its **Locked** tick working. Select it in the Hierarchy to see it or to unlock it, or right-click its row and choose Unlock. A locked object still counts as a surface when you drop something on it or double-click it.

## Scenes

A scene is one level or screen of your game, saved as one file in the `scenes` folder. The Hierarchy lists every scene of the project, each with the clapperboard icon. The highlighted one is your working scene, and the name after **Viewport /** says the same. A closed scene has a dim name.

Click a scene in the Hierarchy, or in the `scenes` folder at the top of Project Assets, to work on it. This is like changing tabs in a text editor: nothing is saved, nothing asks to be saved, and your unsaved edits stay until you save. **Save** writes every open scene to its own file. Undo history starts again each time you change scene.

The Scene View shows only the working scene, new objects go in it, the Environment menu edits it, and Play runs it. The arrow on a scene's row folds its objects, and on a closed scene lists them without leaving the scene you are in. Clicking one of those objects makes its scene the working one.

To work on one scene with nothing else listed, right-click it and choose **Open Only This Scene**. This is the one way of changing scene that can ask you to save, because it closes the others. To close one, right-click it and choose **Close Scene**, or **Close** in Project Assets. A small flag in Project Assets marks the scene the game starts on. Right-click a scene there and choose **Set as Start Scene** to change it.

A flag means the game starts on this scene. The flag isn't a button.

**+** at the top of the Hierarchy makes a new scene, a group or an object. A new scene becomes your working scene, and the others stay open.

Right-click a scene to rename it, lock it or move it to a group:

- **Rename:** this renames the file and updates the project, scene rules and prefabs that use it. Lua or Rust scripts stay as they are, and Studio lists any that still use the old name. The loading screen keeps its name.
- **Lock Scene:** this locks every object in the scene, and **Unlock Scene** unlocks every one, including objects you had locked one by one. You can still add new objects to a locked scene, and a new object isn't locked.
- **Groups:** a group is a folder inside `scenes`. Moving a scene to a group moves its file. Groups only tidy the list, and they don't change the scene order. Deleting a group keeps its scenes.

For the scene order and the loading screen, open the Scenes window: **File → Scenes…**, or **Scene Order…** on the `scenes` folder.

| In the Scenes window | What it does |
| --- | --- |
| Click a scene's name | Open it. Studio asks first if you have unsaved changes. Undo starts fresh in the scene you open |
| **Set as start** | The game starts on this scene when exported. **Game starts here** marks the current one |
| **↑** and **↓** | Move the scene in the scene order. "Go to the next scene" follows this list |
| **Open beside** | List the scene's objects in the Hierarchy too, without leaving the scene you are in |
| **Close** | Close a scene opened beside another. Its file stays |
| **New Loading Screen** | Make the loading screen scene and open it. Shown only if the project has none |
| **New Scene…** | Make a new empty scene with a name you choose, and open it |
| **Save Scene As…** | Save the open scene under a new name and carry on in the new file. The old file stays as last saved |

A name already in use is refused, so a scene is never written over. **Play** runs the working scene, the one named after **Viewport /**. Other open scenes are not in the game unless it loads them.

### Several scenes open

The header shows `main + 1` when one more scene is open. The Hierarchy has a row for each scene with its objects under it, and when you select an object a label at the top of the view says which scene it is in. **Save** writes each scene to its own file.

- New objects go in the main scene. Drag a row onto another scene's row to send it there, or right-click a top-level object and choose **Move to Scene**. Children go with it.
- Lighting, environment and the nav grid shown are the main scene's. A scene opened beside it keeps its own in its file.
- Undo starts fresh when a scene opens or closes.
- Opening a scene the normal way closes any opened beside it.
- A scene made with **Save Scene As…** has the same objects as the original, so the two can't open together.
- Scenes opened beside aren't reopened next time you start Studio, and crash recovery covers only the main scene.

### Moving between scenes in the game

| What you want | Rule action | Lua | Rust |
| --- | --- | --- | --- |
| Replace this scene with another | **Change scene** | `engine.change_scene(path)` | `request_scene_change(path)` |
| Add a scene on top; everything here stays | **Load a scene on top** | `engine.load_scene(path)` | `request_scene_load(path)` |
| Remove a scene loaded on top | **Unload a scene** | `engine.unload_scene(path)` | `request_scene_unload(path)` |
| Go to the next scene in the order | **Go to the next scene** | `engine.next_scene()` | `request_next_scene()` |

A scene loaded on top brings its objects, scripts, rules and Game UI. Its lighting, environment and nav grid are not used. Changing scene removes everything loaded on top, except objects with **Keep between scenes**. Asking for the next scene after the last one does nothing.

### Loading screen

If the project has a scene at `scenes/loading.ron`, its Game UI covers the game while another scene loads. **New Loading Screen** in the Scenes window makes one, a dark panel with "Loading…", and you can change it like any Game UI. It isn't used when loading a scene on top. The game doesn't move while the screen is up, and there is no progress bar.

## Prefab copies and kept objects

Dropping a prefab places a linked copy, with everything under it. Its Hierarchy icon is in the accent colour. The Inspector shows the prefab's file and what this copy changes, with **Apply to prefab** and **Revert**. If the file has gone, it says **Prefab missing** and the copy keeps working.

A pin at the end of a Hierarchy row marks a top-level object with **Keep between scenes** ticked. It stays when the game changes scene, and you see this by playing, not in the Scene View: open a different scene and the object isn't listed there.

## Disabled objects

A disabled object is left out of the Scene View, and so is everything under it, as in the game. In the game it also has no physics, and its scripts and rules wait; a rule or script can turn it back on.

To see it while you work, select it or something inside it in the Hierarchy, and it shows until you select something else. Its row stays in the Hierarchy, greyed out. A disabled object can't be clicked in the Scene View.

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
