# RustyEngine Studio — Keyboard and Mouse Shortcuts

This reference lists every shortcut and mouse action available in RustyEngine Studio, grouped by where you use it.

**Modifier keys.** **⌘** means **Cmd** on macOS and **Ctrl** on Windows and Linux. **⇧** is Shift and **⌥** is Option (Alt on Windows and Linux).

**Focus.** Viewport shortcuts work while the mouse pointer is over the Scene View. Keys typed into a text field (a name, a search box, a number) go to that field, not to the shortcut.

---

## Application

| Shortcut | Action | Notes |
| --- | --- | --- |
| ⌘N | New Project | macOS application menu |
| ⌘O | Open Project | macOS application menu |
| ⌘Q | Quit Studio | macOS application menu; asks first if there are unsaved changes |
| ⌘Z | Undo | Undoes the last scene edit, including terrain sculpting and node moves in the Gameplay Graph |
| ⌘⇧Z or ⌘Y | Redo | |
| ⌘S | Save the open code file | Only when the Code panel is open |
| ⌘, | Settings | Also **File → Settings…**; see [Storage](03_STORAGE.md) |

To save the scene, use **File → Save Scene**. There is no keyboard shortcut for it yet.

---

## Scene View — camera

| Action | 3D view | 2D view |
| --- | --- | --- |
| Orbit (rotate the view) around the point you are looking at | ⌘ + click-drag, ⌘ + two-finger scroll on a trackpad, or right-drag | — |
| Pan (slide the view) | Two-finger scroll on a trackpad, middle-drag, or ⇧ + right-drag | Two-finger scroll, right-drag or middle-drag |
| Zoom toward the mouse pointer | Mouse scroll wheel or trackpad pinch | Same as 3D |
| Glide to a spot | Double-click it (terrain, a model, any surface) | Double-click it |
| Fly forward / back | Hold right mouse button + W / S | — |
| Fly left / right | Hold right mouse button + A / D | — |
| Fly down / up | Hold right mouse button + Q / E | — |
| Fly faster | Hold ⇧ while flying | — |
| Look around while flying | Move the mouse while holding the right button and a fly key | — |
| Frame the selection | **F** | **F** |

The 3D and 2D views each remember their own camera position. Switching between them returns you to where you were.

**Trackpad and mouse.** Studio tells the two apart. On a trackpad (or Magic Mouse): two-finger scroll pans, ⌘ + two-finger scroll rotates, and pinch zooms. With a mouse, the scroll wheel zooms. In the 2D view, ⌘ + scroll pans because the 2D view does not rotate.

**Zooming close up.** Zooming in never stops at an object: once the camera is about 1 m from the point it is looking at, zooming in keeps carrying it forward, so you can zoom past objects and through the scene. Zoom out to back away again.

**Getting somewhere quickly.** Double-click the place you want to work on, such as a spot on the terrain, and the camera glides to look at it. Press **F** to glide to the selected object instead. Any camera movement of your own stops a glide.

---

## Scene View — selecting and editing

| Action | How |
| --- | --- |
| Select an object | Click its model, sprite or icon (locked objects cannot be clicked; select them in the Hierarchy) |
| Add to or remove from the selection | ⌘ + click (without dragging; ⌘ + drag rotates the view) |
| Clear the selection | Click empty space (for example, the sky) |
| Move along an axis | Drag an arrow of the gizmo: red = X, green = Y, blue = Z |
| Rotate around an axis | Drag a ring of the gizmo, like turning a dial |
| Scale along one axis | Drag the small square on that axis's arrow |
| Scale evenly | Drag the white square at the centre of the gizmo |
| Select tool | **V**, or the arrow button at the top right of the viewport |
| Hand tool (left-drag moves the view) | **H**, or the hand button |
| Hand tool while held | Hold **Space**; release to return to Select |
| Add a model, image or prefab to the scene | Drag it from Project Assets into the Scene View |
| Sculpt terrain | With **Terrain → Sculpt in Scene View** on, left-drag on the selected terrain |
| Paint tiles | With **Tilemap → Paint in Scene View** on (2D view), click or drag on the selected tilemap |

While terrain sculpting or tile painting is on, left clicks go to the brush instead of selecting objects.

---

## Hierarchy panel

| Action | How |
| --- | --- |
| Select an object | Click its row |
| Add to or remove from the selection | ⌘ + click |
| Select a range of rows | ⇧ + click |
| Frame the object in the Scene View | Double-click its row |
| Rename the selected object | **F2**, then **Enter** to confirm or **Esc** to cancel |
| Make an object a child of another | Drag its row onto the other row |
| Reorder objects | Drag a row between two other rows |
| Group the selected objects | **⌘G** (works in the Scene View too) |
| Ungroup the selected group | **⇧⌘G** |
| Rename, Duplicate, Group, Ungroup, Make Prefab, Enable/Disable, Lock/Unlock, Delete | Right-click a row |
| Create a new object | **+** button at the top of the panel |

Selecting an object in the Scene View highlights its row in the Hierarchy, scrolls to it, and expands its parents.

**Folding:** every object that has children (and every group) starts folded. Click the arrow next to a row to unfold it. Studio remembers which rows you unfolded, per project, and shows them the same way the next time you open the project. A search unfolds the rows that match only while you search.

### Groups

A group keeps related objects together, like a folder. For example, put every tree into a group called "Trees" so the Hierarchy stays short.

- **Make a group:** select the objects (⌘ + click or ⇧ + click), then press **⌘G**. A new object called "Group" appears at the centre of the selected objects, with them inside it. Its name is ready to edit, so type a name and press **Enter**. Nothing moves in the scene.
- **What a group is:** an empty object. It has a position but no shape. Moving, rotating or scaling the group moves everything inside it. Click the arrow next to it to fold or unfold its contents.
- **Folder icon:** a folder icon marks an empty object that has children. An object with its own content (a model, light, script and so on) that has children keeps its normal icon.
- **Ungroup:** select a group and press **⇧⌘G**. Its objects move out to where the group was, in the same order and at the same place in the scene, and the empty group is removed.
- **Locked objects** are left out of a new group, and a locked group cannot be ungrouped. Unlock them first.
- If the selected objects have different parents, the new group is placed at the top level of the scene.
- **⌘Z** undoes grouping and ungrouping.

The same commands are in the **+** menu at the top of the Hierarchy and in the **GameObject** menu.

---

## Project Assets panel

Folders start folded, including the top-level `assets` folder. Click the arrow next to a folder to unfold it. Studio remembers which folders you unfolded, per project.

The preview of the selected file appears beside the file list when the panel is wide, and under it when the panel is narrow, so file names stay readable. Drag the edge between them to resize the preview.

| Action | How |
| --- | --- |
| Confirm a new folder name or a rename | **Enter** |
| Cancel naming | **Esc** |
| Move a file or folder | Drag it onto a folder |
| Rename, delete and other actions | Right-click an item |

---

## Gameplay Graph

| Action | How |
| --- | --- |
| Move a node | Drag it |
| Move an event together with its actions | Drag the event node |
| Pan the canvas | Drag empty space |
| Connect an action to an event | Click the event, then click the action |
| Return the canvas to its starting position | **Reset view** |
| Return every node to the automatic layout | **Auto layout** |

---

## Code panel

The Code panel uses the Monaco editor, so standard code-editor shortcuts work there. Common ones:

| Shortcut | Action |
| --- | --- |
| ⌘S | Save the file |
| ⌘F | Find |
| ⌘Z / ⌘⇧Z | Undo / Redo inside the file |

---

## Playing in the Scene View

| Key | Action |
| --- | --- |
| **⌘P** | Play the game inside the Scene View, or stop it |
| **⇧⌘P** | Pause or resume the game |
| **⌥⌘P** | While paused: advance exactly one frame |
| Click the game | Give it the keyboard and mouse |
| Click any other panel | Take the keyboard back for Studio |

While the game has the keyboard, every key goes to the game, including **Esc** (games often use it for a pause menu). Only the three ⌘P shortcuts above stay with Studio, so you can always pause or stop. See the [Scene View Guide](02_SCENE_VIEW_GUIDE.md#playing-in-the-scene-view).

---

## Running game

Click **Run** (top bar) to start the game in its own window. Studio switches the bottom panel to the **Output** tab, which shows build progress and anything the game prints. A Rust game compiles only its own code first: the first Run on a machine takes a few minutes, later Runs take seconds. A Lua game starts at once. Click **Stop** to close it.

| Key | Action |
| --- | --- |
| Esc | Close the game window (Run and exported games) |

---

## Not available yet

These common editor shortcuts are planned but not implemented. Use the listed alternative for now.

| Shortcut | Alternative |
| --- | --- |
| ⌘S to save the scene | File → Save Scene |
| Delete / Backspace to delete the selection | Right-click the object in the Hierarchy → Delete |
| ⌘D to duplicate | Right-click the object in the Hierarchy → Duplicate |

