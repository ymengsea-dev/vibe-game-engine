# Keyboard and Mouse Shortcuts

⌘ = Cmd on Mac, Ctrl on Windows and Linux. ⇧ = Shift. ⌥ = Option (Alt).
Shortcuts don't work while you type in a text box.

## Anywhere

| Shortcut | What it does |
| --- | --- |
| ⌘N | New project |
| ⌘O | Open project |
| ⌘S | Save |
| ⌘Z | Undo |
| ⌘⇧Z or ⌘Y | Redo |
| ⌘, | Settings |
| ⌘B | Build and Run: export the game into the project's `Builds` folder and start it |
| ⌘Q | Quit |

## Scene View: moving the view

| Shortcut | What it does |
| --- | --- |
| Right-drag, or ⌘ + drag | Turn the view (3D only) |
| Two-finger scroll, middle-drag, or ⇧ + right-drag | Slide the view |
| Scroll wheel or pinch | Zoom |
| Hold right mouse + W A S D | Fly forward, left, back, right (3D only) |
| Hold right mouse + Q / E | Fly down / up |
| Hold ⇧ while flying | Fly faster |
| Double-click a spot | Move the view to that spot |
| F | Show the selected object (or the whole scene if nothing is selected) |

## Scene View: editing

| Shortcut | What it does |
| --- | --- |
| Click | Select an object |
| ⌘ + click | Add to or remove from the selection |
| Drag an arrow | Move the object |
| Drag a ring | Turn the object |
| Drag a small square | Stretch the object |
| Drag the white centre square | Make the whole object bigger or smaller |
| Hold ⌥ while dragging a handle | Flip snapping for this drag: move, turn and scale in steps when **Snap** is off, freely when it is on |
| ⌘↓ or End | Drop the selected objects onto the ground or object below them |
| V | Select tool |
| H, or hold Space | Hand tool (drag moves the view) |
| ⌫ or Delete | Delete the selected objects |
| ⌘G | Group the selected objects |
| ⇧⌘G | Ungroup |
| Drag on the selected terrain (Terrain menu set to Sculpt, Paint, Foliage or Objects) | Sculpt, paint, add plants or add objects under the blue brush ring. Clicks don't select while the brush is on; set the menu to Off |
| ⇧-drag on the selected terrain (Terrain menu set to Foliage or Objects) | Erase plants or painted objects under the brush ring |
| Drag a dot on the selected road | Move that point of the road over the ground |
| ⌥ + click the ground with a road selected | Add a point to the road: between two points if you click beside the road, else at the nearer end |
| Drag a file from Project Assets | Add it to the scene where the marker shows: on the object under the cursor, else on the ground, else in front of the camera. A model stands on its bottom there (Snap → **Dropped models stand on the surface**). In the 2D view, at the cursor |

## Hierarchy

| Shortcut | What it does |
| --- | --- |
| Click / ⌘ + click / ⇧ + click | Select one / add one / select a range |
| Double-click a row | Show that object in the Scene View |
| F2 | Rename (Enter to keep, Esc to cancel) |
| Drag a row onto another row | Put it inside that object |
| Drag a row between rows | Change the order |
| Drag a file from Project Assets onto a row | Add it inside that object, at its position. Onto the main scene's row or the space below the rows: add it at the top level, at the world's centre |
| Drag a row onto a scene's row | Take it out of its parent, to the top level of that scene |
| Click a scene's row | Work on that scene: the Scene View shows it, new objects go in it and Play runs it. Nothing is saved, and unsaved edits stay |
| Click the arrow on a scene's row | Fold or unfold its objects. On a closed scene, list its objects without leaving the scene you are in. With only one scene in the project there is no arrow |
| Click an object in another scene | Select it and work on its scene |
| Right-click a scene's row | Open Only This Scene, Rename, Move to Group, Set as Start Scene, Lock Scene, New Scene, Save Scene As, Scenes, and Close Scene on one opened beside |
| Click a group's row | Fold or unfold the group |
| Right-click a group's row | Rename Group, Delete Group. Deleting a group keeps its scenes |
| Click **+** | New Scene, New Group, new objects, and actions on the selection |
| ⌘D | Duplicate the selected objects, next to the originals |
| ⌘C / ⌘V | Copy the selected objects / paste them next to the selected object, or at the scene root if nothing is selected. Works between scenes of a project |
| Drag a selected row | Move all the selected objects together. Locked ones stay behind |
| Click the eye at the end of a row | Hide the object and what is under it in the Scene View, or show it again. On a selected row, it does this for the whole selection |
| Right-click a row → Create | Add an empty object, camera, point light, spot light, sound, road, sprite or Game UI element inside that object. A Game UI element made inside another one sits inside it |
| Right-click below the rows | Add a new object at the scene root, or paste |
| Right-click a top-level row → Move to Scene | With several scenes open, send the object to another one |
| F2, Delete or Backspace on a locked object | Nothing. Unlock it first |
| Right-click a row | More options (Duplicate, Copy, Paste, Make Prefab, Lock, Disable…). Make Prefab saves the object with everything under it, and the object becomes a linked copy of the new file |

The icon at the start of a row shows what the object is:

| Icon | Object |
| --- | --- |
| Camera | A camera |
| Sun with rays | A point or spot light |
| Speaker | A sound |
| Rounded button | A Game UI element, such as a label, button, bar or text field |
| Grid | A tilemap |
| Waves | Water |
| Road running away | A road or path |
| Picture | A sprite |
| Ridge | Terrain |
| Leaf | A foliage group |
| Cube | A model |
| Folder | A group |
| Diamond | Anything else, such as an empty object |

An object that is several things shows the first match in this list. Terrain shows the ridge; so does a model in a `terrain` folder from an older Studio, until its scene is opened and converted. The scene's sun has no row, because it is a setting in the Scene View's Environment menu.

## Inspector

| Shortcut | What it does |
| --- | --- |
| Drag sideways on a number | Change the number |
| Click a number, type, Enter | Type an exact number |
| Tick **Locked** | Make the object read-only until you untick it |
| Type in **Add tag**, Enter | Add a tag to the object |
| Click a tag | Remove it |
| Tick **Keep between scenes** | Keep this object when the game changes scene. Only top-level objects have it; children go with their parent |
| Click **Apply to prefab** | Make the prefab and its other copies in this scene match this copy, except its position. Other scenes catch up when opened or played. This can't be undone |
| Click **Revert** | Put this copy back to what the prefab says, keeping its position. Objects you added to it are removed. Undo brings them back |

## Project Assets

| Shortcut | What it does |
| --- | --- |
| Click a folder's row | Fold or unfold it, and pick it as the folder new files and folders go in. This includes the `scenes` folder and its groups |
| Drag onto a folder | Move a file |
| Drag a folder onto the top or bottom edge of another folder | Put it before or after that folder (a line shows where). In another folder it moves there too. The bottom edge of an open folder puts it inside. The order is kept when Studio reopens; folders you haven't placed follow by name |
| Enter / Esc | Keep / cancel a new name |
| Right-click | More options |
| Right-click a folder → **New Material** | Make a material file there |
| Right-click a folder → **New Gameplay Graph** | Make a gameplay graph there and open it ([Gameplay Graphs](07_GAMEPLAY_GRAPHS.md)) |
| Click a `.graph` file | Open it in the Gameplay graph window |
| Drag a material onto a model in the Scene View or the Hierarchy | Draw the model with that material ([Materials](06_MATERIALS.md)) |

## Console

| Shortcut | What it does |
| --- | --- |
| Right-click a message | Copy it |

## Rules graph

Open it from the Inspector's Rules section: **Open rules graph**. It shows the object's visual rules as boxes.

| Shortcut | What it does |
| --- | --- |
| Drag a box | Move it |
| Drag empty space | Move the view |
| Click an event, then an action | Connect them |

## Gameplay graph window

Open it from the Inspector's **Gameplay Graph** section (**Edit graph**), or by clicking a `.graph` file in Project Assets. See [Gameplay Graphs](07_GAMEPLAY_GRAPHS.md).

| Shortcut | What it does |
| --- | --- |
| Click a node | Select it and show its settings below |
| Drag a node | Move it |
| Drag empty space | Move the view |
| Drag from an output dot to an input dot of the same colour | Join them with a wire |
| Drag a wire off an input dot | Pick it up to move it, or drop it on empty space to remove it |
| Right-click empty space, or **+ Node** | Add a node; type to search |
| **+ Comment** | Add a note behind the nodes; drag its title strip to move it and its corner to resize it |
| ⌘C / ⌘V, or **Copy** / **Paste** | Copy the selected node and paste it beside the original |
| **Delete** or **Backspace**, or the **Delete** button | Remove the selected node or comment |
| ⌘Z | Undo the last change to the graph (after the mouse is released) |
| **Reset view** | Move the view back to the start |

## Behaviour tree window

Open it from the Inspector: **AI → Brain → Behaviour tree → Edit behaviour tree**.

| Shortcut | What it does |
| --- | --- |
| Click a node | Select it and show its settings below |
| Drag a node | Move it (the layout saves with the scene) |
| Drag empty space | Move the view |
| **+ Node** | Add a node under the selected Sequence, Selector or Not, or after the selected node |
| **◀** / **▶** | Run the selected node earlier or later among its siblings |
| **Delete** | Remove the selected node and everything under it (not the top node) |
| **Tidy** | Put every node back in the automatic layout |
| **Add point here** (Patrol) | Add the object's current position as a patrol point |

While the game plays, the window can't be changed. With **Play** in the Scene View, it shows what each node of the selected object's tree answered on the last frame: a yellow dot for still running, green for worked, red for failed. Nodes the tree didn't reach are faded. Select the object and open the window before pressing Play: the Hierarchy and Inspector are locked while playing.

## Shader graph window

Open it from a material file's Inspector: **Surface → Shader graph**, then **Edit graph**.

| Shortcut | What it does |
| --- | --- |
| Click a node | Select it and show its settings below |
| Drag a node | Move it |
| Drag empty space | Move the view |
| Drag from an output dot to an input dot | Join them with a wire; an input takes one wire, so a new one replaces the old |
| Drag a wire off an input dot | Pick it up to move it to another input, or drop it on empty space to remove it |
| Right-click empty space | Add a node there; type to search |
| **+ Node** | Add a node in the middle of the view; type to search |
| **Delete** or **Backspace**, or the **Delete** button | Remove the selected node (not Surface) |
| **Reset view** | Move the view back to the start |

## Code panel

| Shortcut | What it does |
| --- | --- |
| ⌘S | Save the file |
| ⌘F | Find |

The file list on the left of the Code screen:

| Shortcut | What it does |
| --- | --- |
| Click a file | Open it |
| Click a folder | Open or fold it. New files go in the folder you clicked last (or beside the file you clicked) |
| New file / New folder buttons at the top | Type a name, then Enter to make it or Esc to cancel. Clicking away keeps a good name. A name that is already there shows a red note |
| Fold all button | Fold every folder |
| Click the project name or empty space in the list | New files go in the project folder itself |
| ↑ ↓ | Move through the list |
| → / ← | Open / fold a folder (← on a file goes to its folder) |
| Enter or Space | Open a file, or open or fold a folder |
| Type in **Find a file** (the box in the middle of the top bar) | Show only matching files and pick the first; ↑ ↓ pick another, Enter opens it, Esc clears the box |

The Viewport on the right of the Code screen shows the Scene View's camera:

| Shortcut | What it does |
| --- | --- |
| Drag or right-drag | Turn around (in 2D: move) |
| Middle-drag, Shift+right-drag or two-finger scroll | Move |
| Mouse wheel or pinch | Zoom toward the cursor |
| F | Frame the selected object |

Moving the camera here moves the Scene View's camera too. Objects can't be clicked or moved in this Viewport.

## Playing the game

| Shortcut | What it does |
| --- | --- |
| ⌘P | Play in the Scene View, or stop |
| ⇧⌘P | Pause or go on |
| ⌥⌘P | When paused: move one frame ahead |
| Click the game | Give the game the keyboard and mouse |
| Esc | Free the mouse if the game holds it (the game keeps running); click the game to lock it again (that click does not count as a click in the game) |
| Esc (game in its own window) | Close the game |

While the game is paused in the Scene View, the usual Scene View moves look around the frozen game with a free camera. It starts where the game's camera was:

| Shortcut | What it does |
| --- | --- |
| Right-drag, ⌘+drag or ⌘+two-finger scroll | Orbit |
| Middle-drag, Shift+right-drag or two-finger scroll | Pan |
| Mouse wheel or pinch | Zoom toward the cursor |
| Hold right mouse + W A S D / Q E | Fly (Shift to go faster) |
| **Back to game camera** | Look through the game's camera again |

⏭ Step keeps the free camera. Resuming goes back to the game's camera.

Players use these on the game's own UI:

| Input | What it does |
| --- | --- |
| Click a button | Click it |
| Drag a slider | Change its value. The variable it is set to follows |
| Click a toggle | Turn it on or off. Its variable becomes 1 or 0 |
| Click a text field | Start typing. Keys go to the field and not to the game until typing ends |
| Backspace (typing) | Remove the last character |
| Enter (typing) | Finish typing; this counts as clicking the field |
| Esc (typing), or click elsewhere | Finish typing without counting as a click. Esc does not also close the game or reach it |
| Arrow keys, D-pad or left stick | Move between buttons, sliders, toggles and text fields. Left and right change a selected slider instead |
| Enter, Space or the gamepad's bottom button | Use the selected button, toggle or text field |

Typed text has no cursor to move. The built-in font (Inter) shows Latin, Greek and Cyrillic letters, numbers and common symbols; other characters, such as Khmer or Chinese, show as a box.

## Project Assets: the scenes folder

| Shortcut | What it does |
| --- | --- |
| Click a scene | Work on that scene. Nothing is saved, and unsaved edits stay |
| Right-click a scene | Open Only This Scene, Rename, Move to Group, Set as Start Scene, and Close on one open beside your working scene |
| Right-click a group | Rename Group, Delete Group |
| Right-click the `scenes` folder | New Scene, New Group, Scene Order |

## Scenes window

Open it from **File → Scenes…**, from the right-click menu of a scene's Hierarchy row, or from **Scene Order…** on the `scenes` folder in Project Assets.

| Shortcut | What it does |
| --- | --- |
| Click a scene's name | Work on that scene |
| Click **Set as start** | Make the game start on that scene |
| Click **↑** or **↓** | Move the scene in the scene order |
| Click **Open beside** | List the scene's objects in the Hierarchy too, without leaving the scene you are in |
| Click **Close** | Close a scene opened beside another |
| Enter in the name box | Create or save the scene you named |
| Esc | Close |

## Not available yet

Nothing is on this list right now.
