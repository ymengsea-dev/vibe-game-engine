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

With **Snap** on (bottom bar), a move puts the object's position on a grid of the move step, a turn goes in whole turn steps and a resize in scale steps. Holding ⌥ while you drag flips this for that drag. With several objects selected, the first one lands on the grid and the others move by the same amount.

In 2D you get two arrows and one ring.

## Locked objects

When you drag a file from Project Assets over the Scene View, a ring with the file's name shows where it will land: on the object under the cursor (locked ones count, so a locked terrain works), else on the ground, else in front of the camera. In the 2D view it lands at the cursor. A model stands on its bottom on that point, as worked out from its mesh (Snap → **Dropped models stand on the surface**; untick it to put the model's own centre there instead). Prefabs and objects without a mesh go by their centre.

**Drop to Ground** (⌘↓ or End, also in the Snap menu and GameObject menu) moves each selected object straight down until the bottom of its mesh rests on the terrain, a road or an object below it. An object sunk inside a terrain hill comes up onto it. An object with nothing below it stays where it is. Objects inside a selected group move with the group, judged by the group's own mesh (or its centre). If a prefab can't be loaded, the Problems tab says why.

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

### World areas (streaming)

A big world can be split into areas that load only while the game camera is near, so the game keeps just the nearby parts in memory.

1. Put each part of the world in its own scene (for example `scenes/forest.ron`), placed where it belongs in the world.
2. In the main scene, add an empty object where the area is, and add **World Area** to it from the Inspector's **Add component**.
3. Pick its **Scene**, set the **Size** of its box and **Load within**: how close to the box the game camera must come for the scene to load.

| Setting | What it does |
| --- | --- |
| **Scene** | The scene that holds this part of the world |
| **Size** | The box the area covers. It is centred on the object |
| **Load within** | How close to the box the game camera must come for the scene to load. It unloads once the camera is a little farther away (10 % more, at least 2 m), so standing at the edge doesn't load it over and over |

While playing, at most one area loads per frame, the nearest first. Model files stay loaded after an area unloads, so coming back is quicker. A selected area shows its box and, fainter, the box where it starts loading; **Overlays → World areas** shows every area. To edit an area's scene beside the main one, use **Open beside** in the Scenes window.

Limits:

- The box doesn't turn or scale with its object.
- An area's own lighting, environment and nav grid are not used, as with any scene loaded on top.
- Areas load around the game camera, not around a chosen object.
- A big area can still pause the game for a moment while it loads.
- A scene a rule or script loaded on top is left alone by areas.

### Loading screen

If the project has a scene at `scenes/loading.ron`, its Game UI covers the game while another scene loads. **New Loading Screen** in the Scenes window makes one, a dark panel with "Loading…", and you can change it like any Game UI. It isn't used when loading a scene on top. The game doesn't move while the screen is up, and there is no progress bar.

## Prefab copies and kept objects

Dropping a prefab places a linked copy, with everything under it. Its Hierarchy icon is in the accent colour. The Inspector shows the prefab's file and what this copy changes, with **Apply to prefab** and **Revert**. If the file has gone, it says **Prefab missing** and the copy keeps working.

A pin at the end of a Hierarchy row marks a top-level object with **Keep between scenes** ticked. It stays when the game changes scene, and you see this by playing, not in the Scene View: open a different scene and the object isn't listed there.

## Animated characters

A model with a skeleton stands in the first frame of its clip; nothing moves while you build. Select it and use the Inspector's **Animation** section: **Clip** picks which animation, **Loop**, **Speed** and **Play on start** say how it plays in the game, and **Preview → Play** plays it in the Scene View until you press **Stop** (a clip without Loop stops at its end). The preview is never saved and does not move the object. Click-to-select and the orange box use the model's resting pose, not the moving one.

**Use animator** replaces the single clip with a state graph and opens the **Animator** window (**Edit animator** opens it again). The start state is marked ▶. **+ State** adds a state; **+ Transition**, then a click on another state, adds a transition from the selected state (select **Any state** first for one that can leave every state). Below the graph, a state picks one clip or **Blend by a number** (several clips, each at a value of a number such as `speed`); a transition sets its **Blend time**, **Wait for end**, and **Only if** checks on numbers. Drag states to arrange them and empty space to pan. Animator changes are scene edits: ⌘Z undoes them and Save keeps them. The Scene View preview plays one clip only; the animator runs in the game.

A 2D sprite cut into a sheet grid can have named **Clips** in the Inspector's Sprite section: a name, first cell, frames, speed and loop each. A name that is empty or already used is not taken.

## Disabled objects

A disabled object is left out of the Scene View, and so is everything under it, as in the game. In the game it also has no physics, and its scripts and rules wait; a rule or script can turn it back on.

To see it while you work, select it or something inside it in the Hierarchy, and it shows until you select something else. Its row stays in the Hierarchy, greyed out. A disabled object can't be clicked in the Scene View.

## Bottom bar

| Button | What it does |
| --- | --- |
| **3D** | Normal 3D view |
| **2D** | Flat view for sprites and tiles. Tile painting works only here. |
| **Global / Local** | Only **Global** works for now |
| **Snap** | **Snap handles to steps**, with the **Move** (metres), **Turn** (degrees) and **Scale** steps; **Dropped models stand on the surface**; **Drop selection to ground**. The menu title reads **Snap: On** while snapping is on |

3D and 2D each remember where you were looking. Studio also remembers your view for each project.

## Overlays

Helper drawings. The game never shows them.

| Overlay | Shows |
| --- | --- |
| **All colliders** | Physics shapes: green, the selected one yellow, triggers purple |
| **Lights** | Each lamp and how far its light reaches; a spot light shows its cone |
| **Cameras** | What each game camera sees |
| **Ground grid** | 1 m squares on the floor |
| **Audio emitters** | Each sound and how far it can be heard |
| **Nav grid** | The walkable area: where characters can walk, lying on the ground (red cross = blocked). Big areas draw every few cells |
| **World areas** | Every world area's box, and fainter, the box where its scene starts loading |
| **Roads** | Every road's centre line, its edges and, fainter, where its soft edge ends. A selected road always shows them |
| **Distance detail** | Draws models as the game will from the Scene View camera: simpler versions far away, and nothing past each object's draw distance, at the project's graphics quality. Off shows every model in full |
| **Cull from game camera** | Skips what the game camera can't see, while you keep moving the Scene View camera freely. The game camera's view is outlined in light blue, and each skipped model gets a faint red box. A badge in the top-left corner counts models drawn, skipped and too far away, plus plants and terrain pieces drawn out of the total. Distance detail and terrain detail are measured from the game camera too. The game camera is the one ticked **Active camera** (or the first camera) |

**Cull from game camera** limits:

- Animated characters and 2D sprites are never skipped, in the game too.
- Skipped models still cast shadows, as in the game.
- Skipped plants and terrain pieces are counted but get no red box.
- The view takes the Scene View panel's shape, as when you press Play there. A game window of another shape sees a little more or less at the sides.

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
| **Create terrain** | Adds a flat 64 × 64 m piece of ground with a collider, so characters and physics stand on it in the game |
| **Create water** | Adds water. Change waves in the Inspector. |
| **Create road** | Adds a road where you are looking, linked to the terrain under it. See [Roads](#roads) |
| **Off / Sculpt / Paint / Foliage / Objects** | **Sculpt**: drag on the selected terrain to shape it with **Raise**, **Lower**, **Smooth** or **Flatten**. **Paint**: drag to paint the chosen layer. **Foliage**: drag to add plants of the model selected in Project Assets; choose **Erase**, or hold Shift, to remove them (with no model selected, Erase removes every kind). **Objects**: drag to add copies of the model or prefab selected in Project Assets as separate objects; **Erase** or Shift removes them the same way. A blue ring shows the brush. **Off** gives the mouse back to selecting and moving. |
| **Radius**, **Strength** | Brush size in metres, and how much one pass changes (the Foliage and Objects brushes have no Strength) |
| **Layers** (Paint) | The selected terrain's ground textures, up to 8. Click a name to paint it. Pick a built-in texture or any image in the project, a tint, and how many metres one copy covers. **Add layer**, **Remove** |
| **Density**, **Spacing**, **Seed** | How the Foliage brush and **Scatter** place plants: Spacing is roughly the metres between them, Density the share of spots that get one, and Seed picks a different pattern. Painting the same spot again adds nothing, and erased plants come back the same when painted again |
| **Objects: Density**, **Spacing**, **Random turn**, **Size … to …**, **Lean with the slope**, **Seed** (Objects) | How the object brush places copies: Spacing is roughly the metres between them and Density the share of spots that get one. Each copy turns by up to **Random turn** degrees (360 = any way) and is sized between the two **Size** numbers. **Lean with the slope** tilts it to the ground. Painting the same spot again adds nothing |
| **Scatter selected model on terrain** | Fills the whole selected terrain with copies of the model selected in Project Assets. They join that model's foliage group on this terrain, if it has one |
| **Tiles: North / East / South / West** | Adds a tile beside the one under the pointer (or the first tile). Its edge joins the ground next to it, and hills fade out across it. Brushes work across tile edges |
| **Remove tile** | Takes the tile out of the terrain. Its file stays in the project, and ⌘Z puts it back |

Each stroke is saved when you let go and is one ⌘Z step. The Inspector's **Terrain** section shows the tiles, the same layer list, and **Detail**: far from the camera the ground is drawn with fewer triangles, and a higher Detail keeps full detail farther away.

Each tile is a `.terrain` file in `terrain/<Name>/`, with its paint in `.splat` files beside it. Rename or move tiles in Project Assets so the paint goes with them; outside Studio, move them together or the tile loses its paint.

Terrains made by older versions of Studio are converted when the scene opens. Studio saves a copy of the scene file as `<scene>.ron.terrain-v1.bak` the first time it saves over it, and leaves the old `.gltf` model in place.

Each model gets one **foliage group** per terrain, shown in the Hierarchy with a leaf. Click any of its plants to select the group. Plants stay on the ground while you sculpt, and ⌘Z puts the ground and the plants back together. Its Inspector shows how many plants it has, its terrain, **See-through** (cut away the clear parts of leaf images), **Two-sided** (see flat leaves from behind) and **Level of detail**, where plants switch to simpler meshes and fade out past 150 m by default (see `04_GRAPHICS_QUALITY.md`).

Painted objects go into one **Painted <name>** group per model or prefab per terrain. Each copy is an ordinary object: click it, move it, give it a collider or a script. Each stands on the bottom of its mesh, and copies move with the ground when you sculpt, as plants do. A group holds at most 2,000 objects.

Limits: textures blend smoothly, not by height. Sculpting while the game plays doesn't change its collision. Moving or rotating a terrain doesn't move its plants. A foliage group holds at most 20,000 plants. Plants have no wind, and grow upright rather than leaning with the slope.

## Roads

A road is an object with a smooth line through its points. Create one with Terrain → **Create road**, GameObject → **Create Road**, or the Hierarchy's Create menu; it starts with two points 12 m apart where you are looking.

With the road selected, its points show as dots in the Scene View. Drag a dot to move that point over the ground. ⌥-click the ground to add a point: between two points when you click beside the road, otherwise at the nearer end. The Inspector's **Road** section has every point's position, **Remove** to take one out, and **Add point** to add one past the end.

| Option | What it does |
| --- | --- |
| **Width**, **Soft edge** | How wide the flat road is, and how far beside it the ground blends back to its own height |
| **Loop** | Joins the last point back to the first |
| **Press into** | The terrain the road levels |
| **Paint**, **Strength** | A layer of that terrain to paint along the road, and how strongly |
| **Apply to terrain** | Levels the ground along the road to the road's height, blending out over the soft edge, and paints the layer. Plants and painted objects there move with the ground. One ⌘Z step undoes it |
| **Show**, **Color**, **Image**, **Repeat** | Draws the road's own surface in the scene and the game, with a colour and an optional image repeated every **Repeat** metres along it |

Moving points doesn't change the ground until you press **Apply to terrain** again. Click the road's surface to select it.

Limits: roads don't join at crossings. The road's surface has no collider (characters walk on the levelled terrain under it). A road on a turned or resized object gets its width from the road settings, not from the object's size. Applying again over an earlier result levels from the ground as it is now, so moving a road away leaves the old cut.

## Navigation

Characters with AI find their way around walls along the scene's **walkable area**. Bake it here; the game does not bake it by itself.

| Option | What it does |
| --- | --- |
| **Cell size** | Size of one square of the area. Smaller finds narrower gaps but makes a bigger area (at most 250,000 squares) |
| **Character radius** | How far characters keep from walls |
| **Character height** | Anything lower overhead blocks the way |
| **Step height** | Ledges up to this high can be stepped over |
| **Steepest slope °** | Ground steeper than this is not walkable |
| **Bake walkable area** | Works out the area from the ground (terrain, or the top of a fixed box collider) and the fixed solid colliders standing on it, then shows it. ⌘Z undoes it |
| **Clear walkable area** | Removes it; characters then walk straight to where they are sent |
| **Show walkable area** | The same as the **Nav grid** overlay |

What counts: only **Fixed** colliders that are not triggers block, by the box around them. Moving bodies, triggers, disabled objects and objects with AI never block. Without any ground the area is flat at height 0 around the solid objects. The area has one floor: a bridge or an upper storey is not walkable.

Bake again after moving walls or reshaping the ground; the area does not follow by itself.

### AI markers

Select an object with an **AI** section to see, in yellow, what it can see (its view angle out to its sight range, at eye height), in purple, how far it hears, and in green, its patrol points and the route between them. Other objects' markers are hidden.

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
| **Create point light** | Adds a lamp that shines all around |
| **Create spot light** | Adds a light that shines a cone, pointing down |
| **Override scene lighting** | Set your own sun: **Sun direction**, **Sun strength**, **Sun color**, **Ambient** (shadow brightness), and **Shadow distance**: how far from the camera the sun casts shadows (60 m unless you change it). With **Time of day** on, the clock aims the sun, so Sun direction is hidden |
| **Start state** | How the game starts: **Playing**, **Menu**, **Paused** or **Game over**. Only the first scene's choice counts. |
| **Fog** | Far things fade into the **Fog color**. **Thickness** is how fast it thickens, **Starts at** how far from the camera it begins. **Ground fog** adds fog lying low, at **Ground fog height**, thinning upwards by **Ground fog fade** |
| **Clouds** | A cloud layer in the sky: **Cover** (0 clear, 1 overcast), **Drift** (how fast it moves while playing) and **Cloud color** |
| **Time of day** | A clock moves the sun and colours the sky and the ambient light: sunrise at 6:00, noon at 12:00, sunset at 18:00. **Start time** is when the scene begins; **Day length** is how many real minutes a whole day takes while playing (0 keeps the time still) |
| **Sky image** | **Use selected image** puts the image picked in Project Assets around the scene as its sky. Use a panorama (2:1), best as `.hdr` or `.exr`. Its average colours above and below the horizon tint the ambient light. **Clear** goes back to the colour sky |

A sound that comes from one thing (footsteps, a waterfall) goes on that object instead: Add component → **Audio Emitter**.

### Sky, fog and time of day

The Scene View shows the sky, fog and clouds as the game will, at the scene's **Start time**. While playing, the clock runs and the clouds drift.

Lua and Rust games can read and set the clock:

```lua
engine.set_time_of_day(18.5)          -- 0 to 24; 18.5 is half past six in the evening
local hours = engine.get_time_of_day() -- nil when the scene has no time of day
```

Rust games call `set_time_of_day(hours)` and `time_of_day()` on the gameplay runtime, or send `GameCommand::SetTimeOfDay`. Setting the time in a scene without time of day turns it on, standing still.

Limits:

- Sprites, particles and 2D scenes aren't fogged.
- Clouds don't move in the Scene View, only while playing.
- One cloud layer. Clouds cast no shadows.
- The sky image doesn't show in reflections, and its lighting is only an average colour.
- With a sky image, the clouds and time-of-day sky colours are hidden; the clock still moves the sun.

## Lights and shadows

The sun always casts shadows. They are sharp near the camera and softer farther away, out to the **Shadow distance**. The project's graphics quality decides how sharp, in the Scene View too (see the Graphics Quality guide).

| Light | Settings |
| --- | --- |
| **Point Light** | **Color**, **Intensity**, **Range**, **Shadows** |
| **Spot Light** | **Color**, **Intensity**, **Range** (it fades to nothing there), **Cone angle** (half the beam's width, in degrees), **Soft edge** (how many of those degrees the edge takes to fade), **Shadows** |

A spot light shines the way its object faces; turn the object to aim it. Add either from Add component, the Hierarchy's Create menu, or the Environment menu.

Tick **Shadows** on the lamps that need them. Only the 4 shadow-casting lights nearest the camera cast shadows at a time, and each costs time every frame (a point light the most, since it looks all six ways).

Limits:

- Animated characters and plants don't cast shadows.
- Up to 16 point lights and 8 spot lights light the scene at a time: the ones nearest the camera.
- Other suns (a second directional light) don't cast shadows.
