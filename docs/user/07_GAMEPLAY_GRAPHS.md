# Gameplay Graphs

A gameplay graph makes an object do things without code: when the game starts, when a key is pressed, when something touches it, and so on. It works the same in Lua and Rust projects, and an object can have a graph and a script at once.

## Making a graph and giving it to an object

1. In **Project Assets**, right-click a folder (or the top folder) and choose **New Gameplay Graph**. Studio makes `New Graph.graph`, with a **Start** and an **Every frame** node, and opens it.
2. Select the object. In the Inspector choose **Add Component → Gameplay Graph**, then drag the `.graph` file from Project Assets onto its **Graph** slot.
3. **Edit graph** in that section, or clicking the file in Project Assets, opens the **Gameplay graph** window.

Many objects can use the same graph file. Each one runs its own copy, with its own variables.

## Reading a graph

- Every graph starts at **event** nodes (red bar). Their white **flow** wires lead to what happens next, one node after another.
- Coloured **data** wires carry values into the nodes that need them: red for true/false, green for numbers, purple for text, orange for vectors (positions and directions), blue for objects.
- A data input without a wire uses the value set on the node: select the node and change it below the canvas.
- An **Object** input left as **This object** means the object running the graph.

## The window

- Drag from an output dot to an input dot to join them. Only dots of the same colour join; the window says why when they don't. A data input takes one wire; a flow output leads to one place.
- Drag a wire off an input to move it, or drop it on empty space to remove it.
- Right-click empty space, or use **+ Node**, to add a node; type to search.
- **+ Comment** adds a note behind the nodes. Drag its title strip to move it, its corner to resize it, and change its text below the canvas.
- **Copy** and **Paste** (or ⌘C and ⌘V) copy the selected node. **Delete** (or the Delete key) removes it.
- **Variables**, on the left, are the graph's own values, kept for each object while the game runs. Use **Get variable** and **Set variable** nodes to read and change them. Renaming one renames it in its nodes.
- Every change is saved straight away. ⌘Z undoes it once you let go of the mouse.
- Red text above the canvas, and a red outline on the node, show what would stop part of the graph working: wires between different kinds of value, a variable that isn't there, or a value that feeds back into itself.

While the game plays in the Scene View with the window open, nodes that ran in about the last half second get a green edge. The window follows the selected object if it runs this graph, or else the first object that does.

## Nodes

| Group | Nodes |
| --- | --- |
| Events | **Start**, **Every frame** (gives the seconds since the last frame), **Key pressed**, **Key released**, **Starts touching**, **Stops touching**, **Trigger entered**, **Trigger left** (give the **Other** object), **Named event**, **Timer** (once or repeating), **Button clicked**, **Variable changed**, **AI** events (sees a target, loses its target, hears a noise, arrives, changes state), **Animation event**, **Custom event** |
| Flow | **Branch** (True or False), **Sequence** (three things in order), **For loop** (Body once per number from First to Last, then Done; at most 1,000 turns), **Delay**, **Do once** (until Reset), **Gate** (Open, Close, Toggle) |
| Values and maths | Fixed **Number**, **True / false**, **Text** and **Vector** values, **This object**, **Add**, **Subtract**, **Multiply**, **Divide**, **Compare**, **And**, **Or**, **Not**, **Random number**, **Make vector**, **Split vector**, **Add vectors**, **Scale vector**, **Distance**, **Join text**, **Number to text** |
| Questions | **Position of**, **Find by tag** (the first object and how many), **Exists**, **Key held**, **Key axis** (−1, 0 or 1 from two keys), **Raycast**, **Line of sight** |
| Variables | **Get variable**, **Set variable** (this graph's), **Get game variable**, **Set game variable** (the number variables rules and scripts share) |
| Actions | **Log**, **Set position**, **Move by**, **Rotate** (degrees), **Show or hide**, **Destroy**, **Spawn prefab** (gives the new object), **Play sound**, **Play animation**, **Set UI text**, **Change scene**, **Push**, **Set velocity**, **Walk to**, **Send event** (to every graph, rule and script), **Call event** (runs a Custom event in an object's graph), **Set game state**, **Shake camera** |

## Example: a coin

On the coin, which has a trigger collider:

**Trigger entered** → **Set game variable** `coins` (Value: **Get game variable** `coins` + 1) → **Destroy** (This object).

## How it runs

- Actions take effect when the event's nodes have finished, like Lua and Rust commands. So a **Position of** read right after **Move by** in the same event still gives the old position; the next event sees the new one.
- One event may run at most 10,000 node steps. A graph that goes past that stops there and the game log names the node.
- Keys run while the game is paused; Every frame, timers and delays wait.
- When the scene changes, every graph starts again with fresh variables, objects kept from the last scene included.
- Rust games run graphs through the shared gameplay runtime, as the starter project does. For the green live edges, a Rust game's `Game` also returns `self.gameplay.graph_trace(object_id)` from `graph_trace` (the starter does).

## Limits

- Copy and paste take one node at a time.
- A graph can't call another graph file as a function; use **Call event** with a Custom event, or **Send event**.
- **Get game variable** and **Set game variable** hold numbers only, as rules and scripts do.
- Visual rules on objects stay as they are; there is no button to turn them into a graph.
