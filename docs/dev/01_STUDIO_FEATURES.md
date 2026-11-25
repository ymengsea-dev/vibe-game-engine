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
| Scenes | The Hierarchy lists every scene of the project as a row with its own icon, and so does a `scenes` folder in Project Assets: click one to work on it, like changing tabs in a text editor, with no save prompt. The Scene View shows only the scene you are working on. Scenes can be renamed, locked and put in groups. The Scenes window (File → Scenes…) makes new ones, saves under a new name, and chooses the one the game starts on. Play runs the open scene. Put scenes in order with the arrows | Build a game with a menu and several levels | ✅ |
| Several scenes open | Open scenes beside each other. The Hierarchy lists objects under each scene's row, the view labels which scene the selected object is in, and Save writes each to its own file | Work on a level and its shared parts together | ✅ |
| Load a scene on top | Rules, Lua and Rust can add a scene to the running game and remove it again, with its objects, scripts, rules and UI | Add an area, a boss room or a shop without leaving the level | ✅ |
| Next scene | Rules, Lua and Rust can go to the next scene in the order | Move through levels without naming each one | ✅ |
| Loading screen | Game UI you make in a scene called `loading` covers the game while another scene loads. There is no progress bar | Hide the wait between levels | ✅ |
| Folder order | In Project Assets, drag a folder above or below another to put folders in your own order. Studio remembers it; new folders go after, by name | Keep the folders you use most at the top | ✅ |
| Storage | Settings (⌘,) shows what each project keeps on disk (build caches, temp files, old engine copies, backups) with Clean buttons; optional automatic cleanup (off by default; 30 days unused, 10 GB cap). Never deletes project files, asset IDs or saves | Stop build caches from filling the disk over time | ✅ |

## 1b. Scene View (building the world)

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Camera navigation | Right-drag, ⌘+drag or ⌘+two-finger scroll to orbit; two-finger trackpad scroll, middle-drag or Shift+right-drag to pan; mouse wheel or pinch to zoom toward the cursor; hold right mouse + WASD/QE to fly; double-click a spot to glide there | Move freely around the world while building it, with a mouse or a trackpad | ✅ |
| Navigation tools | Two tools, as in Figma: Select (V, default) and Hand (H, or hold Space) choose what a plain left-drag does; ⌘ + drag rotates and pinch/wheel zoom with either tool | Move around with one mouse button or trackpad finger | ✅ |
| Frame selected | Press F over the Scene View, or double-click an object in the Hierarchy, to fly the camera to it (F2 renames in the Hierarchy) | Find and focus on an object quickly | ✅ |
| Transform gizmo | One combined gizmo, no modes: arrows move along an axis, a knob on each arrow scales along that axis, faint rotation rings turn like a dial, and the centre square scales evenly; drawn on top at a constant size, hovered handle brightens, dragged handle turns yellow | Move, rotate and resize objects without switching tools | ✅ |
| Snapping | The **Snap** menu in the Scene View's bottom bar makes the handles move in steps (0.5 m), turn in steps (15°) and scale in steps (0.1); you set the steps. Hold ⌥ while dragging to flip snapping for that drag | Line things up neatly on a grid | ✅ |
| Drop to ground | ⌘↓ (or End, or GameObject → Drop to Ground) moves the selected objects straight down, or up out of a hill, until the bottom of each rests on the terrain or object below. Models dragged in from Project Assets stand on their bottom too | Put rocks, crates and buildings on the ground without nudging them by hand | ✅ |
| Object icons | Cameras, point lights and sound sources show a small icon (camera, bulb, speaker) at a fixed size; click an icon to select its object (Cmd-click adds). A red box marks a model that failed to load | Find and pick objects that have no visible shape | ✅ |
| Click to select | Click any model, sprite, plant of a foliage group or icon in the Scene View to select it (exact triangle hit, nearest wins); Cmd-click adds or removes; click empty space to deselect. The Hierarchy scrolls to the row and opens its parents | Pick objects directly in the world | ✅ |
| Delete objects | Select objects and press Backspace or Delete (or right-click → Delete); their children go too, and ⌘Z brings them back | Remove things quickly while building | ✅ |
| Groups | Select objects and press ⌘G to put them in an empty "Group" object, shown as a folder in the Hierarchy; ⇧⌘G ungroups. Nothing moves on screen, and it can be undone | Keep a long Hierarchy tidy (all trees in "Trees") and move related objects together | ✅ |
| Drag into the scene | Drag a file from Project Assets over the Scene View: a marker shows where it will land, on the object under the cursor, on the ground or in front of the camera, and a model stands on its bottom there. Drop it on a Hierarchy row to make it that object's child | Place things where you want them instead of finding them at the world's centre | ✅ |
| Duplicate, copy and paste | ⌘D copies the selected objects next to the originals; ⌘C and ⌘V copy and paste them, also between scenes of a project. Everything an object has comes along, including children | Build levels quickly from what is already there | ✅ |
| Move several objects | Drag a selected row to move the whole selection: into another object, between rows or to the scene root. They stay where they are in the world | Tidy up a long Hierarchy in one go | ✅ |
| Hide in the editor | The eye on a Hierarchy row hides an object and its children in the Scene View only. The game is not affected and the scene does not change. Hidden lights still shine | See and click what is behind something while you work | ✅ |
| Create menu | Right-click a Hierarchy row to add an empty object, camera, point light, sound, road, sprite or Game UI element inside it; right-click below the rows, or use +, to add one at the top level | Add objects where you want them | ✅ |
| Hierarchy icons | Each Hierarchy row has an icon for what the object is: camera, light, sound, on-screen element, tilemap, water, road, sprite, terrain, model or group. Cameras, lights and sounds use the same shapes as the Scene View | Find things in a long list at a glance | ✅ |
| Lock objects | A locked object can't be changed until you unlock it: it can't be clicked or moved in the Scene View, painted, renamed or deleted, and the Inspector is read-only. It still counts as a surface for drops and double-click focus | Protect ground and finished pieces from accidental changes | ✅ |
| Selection highlight | An orange box hugs the selected object (and its children) and its colliders are drawn in orange; other colliders appear only with Overlays → All colliders (off by default, so the scene looks like the game) | See clearly what is selected | ✅ |

## 2. Writing gameplay in Lua

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Lua scripts | Small script files attached to objects in the scene | Making objects move, react to keys, collide, play sounds, play a character's animations, show, hide or remove objects, pause the game, change scenes and keep score | ✅ |
| Math and input in Lua | Scripts can work with 3D positions as vectors, turn, scale and aim objects, and read every key, mouse button and gamepad control, including locking the mouse for look-around (scaling an object does not resize its collider yet) | Spinning objects, first-person style movement, aiming at targets and gamepad support | ✅ |
| Physics in Lua | Scripts can push things, move characters that slide along walls, shoot rays to see what is ahead, and react to triggers such as coin pickups. Each collider has settings in the Inspector: trigger, mass (or Auto), friction, bounciness, gravity and keep upright | Jumping and shoving crates, platformer and top-down characters, ground checks, pickups and goal zones | ✅ |
| Safe sandbox | Scripts cannot touch files, the network or the computer, and are stopped if they run too long or use too much memory | A broken or endless script shows an error instead of freezing the game | ✅ |
| Code editor help | Lua colouring, autocomplete, hover help and error marks before Play | Beginners see mistakes and available commands while typing | ✅ |
| Code screen | The Code screen shows the project's files (with icons, New file and New folder, empty folders too) and a Find a file box in the top bar, the code editor in Studio's colours, and a Viewport you can turn, move and zoom. The editor can't be closed there | Write code while watching the scene | ✅ |
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
| Play in the Scene View | ▶ Play, ⏸ Pause and ⏭ Step in the Scene View header (⌘P, ⇧⌘P, ⌥⌘P): the same game program as Run, shown inside the Scene View with keyboard and mouse passed through. Works for Lua and Rust projects; the scene is read-only while playing. While paused, a free camera looks around the frozen game with the Scene View's moves | Quickly see what the world looks like in the game without leaving the editor, and freeze a moment to look at it from any side | ✅ |
| Export | **File → Build & Export…** makes the game into something players start without Studio: on macOS a `.app` with the game's name and icon, on Windows a folder with `<Name>.exe`, on Linux a folder with the program. It replaces only an earlier export of the same project, and progress shows in the Output tab | Share the finished game with players | ✅ macOS. 🟡 Windows and Linux: written and code-checked, not yet tried on those systems |
| Game settings | **Settings → Project → Game**: the game's name, version, macOS bundle ID, icon, window size, full screen and resizable window. Run and every export use them, Lua and Rust alike | Make the game look finished: its own name, icon and window | ✅ |
| Build and Run | ⌘B (File → Build and Run) exports the selected configuration into the project's `Builds` folder and starts it, with its messages in the Output tab | Try the real exported game in one step | ✅ |
| Studio package | Bundles Studio with everything it needs (player, code editor, Lua helper) for macOS, Windows or Linux, packed as a disk image, zip or tarball. Signed for the building Mac only, unless a Developer ID (and notarization) or a Windows certificate is given | Install Studio on a machine without Rust | ✅ |

## 5. Building 3D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Terrain | Create ground and sculpt hills and valleys with a brush; every stroke can be undone and redone. Characters and physics stand on it in the game | Landscapes for 3D games | ✅ |
| Terrain painting | Paint up to 8 ground textures (grass, rock, dirt, sand built in, or your own images) onto the terrain with a brush; they blend where they meet | Make paths, beaches and cliffs look different | ✅ |
| Large terrains | Add tiles beside a terrain to make the world bigger; far-away ground is drawn with less detail so big worlds stay fast | Open worlds | ✅ |
| Foliage | Scatter trees, grass or rocks over a whole terrain in one click. The plants stay on the ground when you sculpt it, and far away they switch to simpler meshes and fade out | Fill a landscape quickly | ✅ |
| Foliage brush | Paint plants of the model selected in Project Assets onto a terrain with a brush, or erase them (Shift-drag). Painting over the same spot twice doesn't pile them up | Put forests and meadows exactly where you want them | ✅ |
| Object brush | Paint copies of a model or prefab (rocks, crates, fences) onto a terrain with a brush, each turned and sized at random within the ranges you set, optionally leaning with the slope; Shift-drag erases. Each copy is a normal object you can select, move or give a collider, kept in a "Painted …" group. They stay on the ground when you sculpt | Dress a level with props quickly | ✅ |
| Roads and paths | A **Road** object is a smooth line through points you drag in the Scene View (⌥-click adds one). **Apply to terrain** levels the ground along it with a soft edge and paints a ground texture there; the road can also draw its own surface with a colour or image | Roads, tracks and footpaths across a landscape | ✅ |
| See-through leaves | Leaf and grass images with clear parts are cut out, and flat leaves can be seen from behind. Uses the model's own settings, or tick **See-through** and **Two-sided** on a foliage group | Plants that look like plants, not cards | ✅ |
| Water | Add a water surface with waves and currents | Lakes, seas and swimming | ✅ |
| Lighting | Sun, ambient light, point lights and spot lights (a cone you aim by turning the object); with many lights, the 16 point and 8 spot lights that matter most to the camera are used | Set the mood and visibility of a scene | ✅ |
| Shadows | The sun's shadows are sharp near the camera and reach as far as the scene's shadow distance, sharper at higher graphics quality. Tick **Shadows** on point or spot lights: the 4 nearest the camera cast shadows | Objects that sit in the world instead of floating on it | ✅ |
| Cameras | A camera shows the world from its object; mark one as active and the game looks through it. A Camera Follow section makes it trail an object: behind a character, from its eyes (first person, turned by the mouse or stick) or along a 2D level. A key can flip it between first and third person while playing. Scripts can switch cameras and shake the screen | Set up what the player sees | ✅ |
| Environment | Scene-wide settings with no position, in the Scene View's Environment menu: ambient sounds (wind, music) that loop at the same volume everywhere, plus sun and ambient light | Effects that belong to the whole scene, not to one object | ✅ |
| Fog | Far things fade into a colour you pick, starting a set distance from the camera, plus optional ground fog lying low | Depth, mood, and hiding where the world ends | ✅ |
| Clouds | A moving cloud layer in the sky, with cover, drift speed and colour | Skies that feel alive | ✅ |
| Time of day | A clock that moves the sun and changes the sky and light colours, from sunrise to night. Set the start time and how long a day lasts; Lua and Rust games can read and set the time (`engine.set_time_of_day`) | Day and night cycles, evening scenes | ✅ |
| Sky image | Use a panorama image (`.hdr`, `.exr`, `.png`, `.jpg`) as the sky. Its colours tint the scene's ambient light | Realistic skies from photos | ✅ |
| Materials | Change a model's colour, shininess and roughness. Material files (Project Assets → New Material) hold colour, shine, glow, see-through and images (colour, bumps, metal/rough, shadowing, glow) with repeat and shift, and show a preview ball; drag one onto a model to use it. An object can also swap single images | Make imported models look different without editing them, and share one look across many models | ✅ |
| Level of detail | Far away, a model switches to simpler versions of itself: ones named `_LOD1`, `_LOD2`… in the model file, or ones Studio builds when you tick "Generate simpler meshes" in the Inspector. You choose the distance where each one takes over. Animated characters too, with every version following the same skeleton | Big scenes with many models stay fast | ✅ |
| Draw distance | An object stops being drawn past a distance you set in the Inspector, fading out instead of popping. The Scene View's **Distance detail** overlay shows it as the game will. A character past it also stops working out its pose | Skip far-away clutter you can't see anyway | ✅ |
| World streaming | Split a big world into areas, each its own scene. A **World Area** object in the main scene loads its scene while the game camera is near and unloads it after, the nearest first, one per frame | Big open worlds without loading everything at once | ✅ |
| Camera culling view | **Overlays → Cull from game camera** in the Scene View skips what the game camera can't see while you fly around freely. Its view is outlined, skipped models get a faint box, and a badge counts what was drawn and skipped | Check that the game isn't drawing things nobody sees | ✅ |
| Graphics quality | Low, Medium or High in **Settings → Project**: shadow sharpness, how soon simpler models appear and how far objects are drawn. Lua and Rust games can change it while playing (`engine.set_quality`), for an options menu | Run well on slower computers, look sharp on fast ones | ✅ |
| Animated characters | A model with a skeleton (glTF) moves in the scene. The Inspector's Animation section lists its clips, picks one and sets loop, speed and whether it plays on start; Preview plays it in the Scene View. Lua and Rust games switch clips by name with a smooth blend (`engine.play_animation`), change the speed and pause. Each copy of a character uploads its own mesh | Walking, running and jumping characters | ✅ |
| Animator | A state graph on an animated model, edited in the Animator window (Inspector → Animation → Use animator). Each state plays a clip, or blends clips by a number (walk to run by speed). Transitions go to another state when numbers match, optionally after the clip ends, with a blend time. The numbers are the object's own (Lua `engine.set_animator_value`) or the game's variables. Saved with the scene, so ⌘Z undoes it | Characters that pick idle, walk, run and jump by themselves | ✅ |
| Animation events | Named moments stored in a clip (a footstep, a sword hit) reach rules ("Animation event" trigger), the object's Lua script (`on_animation_event`) and Rust games | Footstep sounds and hits in time with the motion | ✅ |

## 5b. Characters with AI

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Walkable area | Scene View → **Navigation → Bake walkable area** works out where characters can walk: the ground (terrain, and the tops of fixed boxes, cylinders and meshes) minus fixed solid objects, steep slopes and gaps. Up to four floors on top of each other, so bridges, ramps and upper storeys work. Solid objects block by their real shape. Moving objects with **Blocks paths** on are walked round while the game runs. In 2D the area lies on the screen and sprite and tilemap colliders block it. Settings: cell size, character radius and height, step height and steepest slope. The **Show walkable area** overlay draws every floor, with blocked cells crossed out. Saved with the scene and undone with ⌘Z. Bake again after moving walls or ground | Let characters find their way around walls, over bridges and round moving crates | ✅ |
| Walking to a place | An object walks to a place along the walkable area, around walls, turning to face where it goes (straight there when the scene has no walkable area). A Kinematic body walks with collisions; other objects follow the ground. Lua `engine.move_to` / `engine.stop_moving`, Rust `GameCommand::MoveTo` / `StopMoving`, or the rule actions "Walk somewhere" and "Stop walking". It reports arriving (`on_move_finished`, rule trigger "Reaches its destination"). Walking characters steer round each other and round AI characters standing still (Inspector: **Radius**, **Avoid others**) | Send a character to a door or a pickup | ✅ |
| AI settings | The Inspector's **AI** section (Add component → AI): walking speed, how close counts as arrived, turn speed, its size and whether it steers round others | Give an object a way to walk and think | ✅ |
| Senses | An AI object looks for objects with a tag (`player` by default): it sees them inside its view range and angle unless a wall is in the way, and hears them within its hearing range through walls. It forgets a target after a few seconds without contact. Noises made with Lua `engine.make_noise` or the rule action "Make a noise" are heard within their own range. Rules ("Notices a target", "Loses its target", "Hears a noise"), Lua (`on_target_seen`, `on_target_lost`, `on_noise_heard`) and Rust (`on_ai_event`) react. The Scene View shows the view and hearing range of the selected object | Guards that spot the player | ✅ |
| Simple brain | Ready-made enemy behaviour: stand still or patrol between points (waiting at each, round or back and forth), chase what it notices, attack when close (sending an event every few seconds), walk to noises it hears, and go back once the target is lost. Each state can play an animation clip. Lua `engine.set_ai_state` / `get_ai_state` and the "AI state changes" rule trigger follow its state | Enemies without writing code | ✅ |
| Behaviour tree | A brain made of nodes in the **Behaviour tree** window (Inspector → AI → Brain → Behaviour tree → Edit behaviour tree): Selector, Sequence and Not; checks (can see, can hear, has a target, target within, variable, chance); actions (move to target, patrol, move to, wait, play animation, send event, set variable, face target, stop). Saved with the object, so it copies with prefabs. While the game plays in the Scene View, the window shows which nodes ran on the last frame and what each answered | Smarter enemies and helpers | ✅ |
| Paths and line of sight in scripts | Lua `engine.find_path(from, to)` returns the corners of a walkable route; `engine.line_of_sight(from, to)` says whether a wall is in the way. Rust games ask `self.gameplay.queries(ctx)` | Custom AI written in Lua or Rust | ✅ |

## 6. Building 2D worlds

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Sprites | Drag an image into the scene to make an object | Characters, items and backgrounds | ✅ |
| Sprite sheet animation | Play frames from a sprite sheet as a flipbook. Named clips ("walk", "jump") each have their own first cell, frames, speed and loop, and games switch between them with `engine.play_animation` | Walking, idle and other simple animations | ✅ |
| Tilemaps | Paint levels from a tile sheet, optionally solid, on layers (Background, Ground, Front and your own) that you can hide, lock, reorder, fade and make solid. Rule tiles pick their picture from the tiles round them, from a 16- or 47-tile block | Platformer and top-down levels | ✅ |
| 2D collision | Box and circle colliders fitted to sprites | Walls, floors, balls and pickups | ✅ |

## 7. Game UI

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Labels and buttons | On-screen text and clickable buttons placed in the editor, with their own font size, bold option, corner radius and colours. Text uses the built-in Inter font, which shows Latin, Greek and Cyrillic; no custom fonts yet | Score displays, menus, start buttons | ✅ |
| Panels and images | Coloured backgrounds and pictures from Project Assets, placed on screen | Menu backgrounds, icons, logos | ✅ |
| Progress bars | A bar that fills by how far a variable lies between two values | Health, stamina, loading | ✅ |
| Sliders and toggles | Controls the player changes with the mouse, keyboard or gamepad; each one is tied to a variable that rules and scripts can read and set | Settings menus such as volume or sound on/off | ✅ |
| Text input | A field the player types into. Pressing Enter counts as clicking it, for rules and for Lua or Rust scripts, which can read the text. No cursor to move or text selection | Player names, chat, cheat codes | ✅ |
| Layouts | A box that lists the UI elements inside it, top to bottom or left to right, with even gaps. An element inside a panel or other UI element moves with it | Menus that stay tidy when items are added or hidden | ✅ |
| Show and hide | Rules and Lua or Rust scripts show, hide, enable or disable any UI element; hiding one also hides what is inside it | Pop-up menus and pause screens | ✅ |

## 8. Gameplay without code

| Feature | What it is | What it's used for | Status |
| --- | --- | --- | --- |
| Visual rules | "When this happens, do that" rules on an object. Triggers: button clicked or Enter pressed in a text field, named event, scene starts, key pressed, touching or leaving another object or a trigger zone, a timer (once or repeating), a variable changed, the game state changed, an animation event, an AI noticing or losing a target, hearing a noise, arriving, or changing state. Actions: change text, play a sound, send an event, set or add to a variable, change scene, show or hide, enable or disable, spawn a saved object, destroy, set the game state, walk somewhere, stop walking, make a noise. A saved object spawned while playing runs its own rules too, starting with its "Scene starts" rules. Rust projects get newer triggers, actions and fixes after upgrading their engine copy (File menu) | Simple game logic without writing a script | ✅ |
| Variables and conditions | Rules can set or add to a number (e.g. `coins`) and run only if up to 8 checks pass, either all of them or any of them (e.g. `coins ≥ 3`); text can show a value with `{coins}` | Scores, counters and win conditions without code; Lua can read and write the same values | ✅ |
| Game state | The game is always in one of four states: menu, playing, paused or game over (it starts as playing). Pausing freezes scripts' update, physics, timers, animations and camera follow and shake. Buttons, events, key-press rules, state-changed rules and sounds already playing keep going, so a menu can unpause. The state carries over to the next scene. Choose a scene's start state in the Scene View's Environment menu. Lua can read and change it, and gets a call when it changes | Pause menus, title screens and game-over screens | ✅ |
| Show, hide and remove | Objects can be hidden, disabled or destroyed by rules or Lua. A disabled object is left out of the Scene View until selected. It is not drawn in the game, has no physics, and its scripts and rules do nothing (Studio's Scene View still shows it). An object that starts disabled works fully once it is enabled: its rules run, its on-screen text or button appears, and its script starts. A disabled object also disables its children in the game. Showing or enabling a parent also does it for children that were hidden on their own. Timers of a disabled object keep counting, so a once-only timer that runs out while it is disabled is used up | Doors, pickups, enemies and pop-up screens | ✅ |
| Rules graph | A canvas of an object's visual rules as event and action nodes joined by curved lines; drag nodes to arrange them (layout saves with the scene), drag empty space to pan, click an event then an action to connect them | See and rewire game logic visually | ✅ |
| Keep between scenes | A tick on a top-level object that keeps it, and everything under it, when the game changes scene: same position, physics, sound, rules and script memory. It does not run its start again, and going back to its scene does not make a second one. Text a script put on its UI goes back to what you typed in Studio | A player, music or a game manager that carries through every level | ✅ |
| Tags | Labels on an object, added in the Inspector. Lua `engine.find_objects_by_tag` and Rust `find_objects_by_tag` return every object with that tag | Find all enemies or pickups at once | ✅ |
| Prefabs with children | Make Prefab saves an object with everything under it, and a dropped or spawned prefab brings it all back with its scripts, colliders, lights and sounds | Reuse a turret with its gun, or a house with its door | ✅ |
| Linked prefab copies | A placed copy follows its prefab file. Change a field and it stays this copy's own; everything else updates when the prefab does. Apply to prefab sends the copy's changes to the prefab and other copies; Revert drops them. Both act on the whole copy. Apply can't be undone. Renaming the file breaks the link, and a prefab inside a prefab is saved as plain objects | Fix one crate and have them all fixed, while one keeps its own colour | ✅ |
| Gameplay graphs | Graph files (Project Assets → New Gameplay Graph) an object runs alongside its script, in Lua and Rust projects: events (start, every frame, keys, touches, triggers, timers, buttons, variables, AI, animation, custom), flow (branch, sequence, loops, delay, do once, gate), values and maths, questions (position, tags, raycast, line of sight), graph and game variables, and actions (move, rotate, spawn, destroy, sound, animation, UI text, scene, push, walk, events). Typed coloured wires, node search, comments, copy and paste, undo, and nodes that light up while the game plays in the Scene View | Bigger games without code | ✅ |
| Shader graph | Switch a material file to Shader graph and build its colour, shine, bumps, glow, see-through and vertex movement from nodes (images, time, maths, noise, rim light). Parameters can differ per object and be set by Lua or Rust scripts | Glowing pulses, water, dissolves and other custom looks without shader code | ✅ |

## 9. Still to do

- Test the 2D and 3D starter projects in the running editor.
- Later (Studio supports macOS for now): package and test Studio on Windows and Linux, including a clean machine without Rust.
