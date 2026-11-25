# RustyEngine Studio — Graphics Quality and Distance

Far-away objects can use simpler models or not be drawn at all, so big scenes stay fast. One quality setting decides how much detail the whole game shows.

---

## Graphics quality

Open **File → Settings…** (or press **⌘,**) and choose **Project**.

| Level | What changes |
| --- | --- |
| **Low** | Softer shadows (sun shadows in 2 steps, small lamp shadows), simpler models sooner, objects drawn less far (60%) |
| **Medium** | The default: sun shadows in 3 sharp steps |
| **High** | Sun shadows in 4 sharp steps and sharper lamp shadows, detailed models for longer, objects drawn further (150%) |

The sun's shadows are cut into steps from the camera out to the scene's **Shadow distance** (Environment menu): the nearest step is the sharpest. The Scene View shows shadows at the project's quality, as the game does.

The setting is saved with the project, and the game starts at it, in Play and in an exported game.

A game can change it while playing, for example from an options menu:

```lua
engine.set_quality("low")      -- "low", "medium" or "high"
local now = engine.get_quality()
```

Rust games use `ctx.set_quality(QualityLevel::Low)` or the `GameCommand::SetQuality` command.

## Level of detail (Inspector)

Select a model. Its **Level of detail** section has:

| Setting | What it does |
| --- | --- |
| **Simplify** — Generate simpler meshes | Studio builds two lighter copies of the model to draw far away. Very small models are left as they are. Hidden when the model file already has its own simpler versions |
| **Level 1 from**, **Level 2 from**… | The camera distance where each simpler version takes over |
| **Draw range** | Tick to stop drawing the object past this distance |
| **Fade** | How far before the draw range it starts to fade out |

**⋯ → Reset** puts everything back to the defaults.

A foliage group has the same section, for all its plants at once. Each plant switches and fades by its own distance from the camera. New groups stop drawing at 150 m with a 10 m fade; **Reset** goes back to that.

### See-through and two-sided leaves

Leaf and grass images usually have clear parts. A model exported with glTF's alpha mode **MASK** has them cut away, and one marked **double-sided** shows its flat leaves from behind too. If a model wasn't exported that way, select its foliage group and tick **See-through** (with a **Cutoff**: pixels less solid than this are cut away) and **Two-sided** in the Inspector's **Foliage** section.

### Simpler versions in your model file

Put each version in the same glTF file as its own mesh, named with `_LOD` and a number: `Rock_LOD0` (full detail), `Rock_LOD1`, `Rock_LOD2`. All must share the same name before `_LOD`. Studio then uses them instead of generating its own.

Animated characters work the same way: each `_LOD` mesh must be skinned to the same skeleton, so every version moves with one pose. **Generate simpler meshes** keeps each vertex's bone weights. Past its draw distance a character isn't drawn and its animation only keeps time, so far crowds cost little.

## Seeing it in the Scene View

Turn on **Overlays → Distance detail** to see simpler models and draw ranges from the Scene View camera, at the project's quality. Off, every model and plant is drawn in full.

With **Overlays → Cull from game camera** also on, distances are measured from the game camera instead, so you can fly around and watch what it would draw.

## Not available yet

- Animated characters are not left out when they are outside the camera's view, only when they are too far away, because a pose can reach outside the model's resting shape. They also cast no shadows yet.
- Plants don't keep the simpler mesh a little longer when the camera hovers at the switch distance, as single models do.
- Terrain draws far-away ground with fewer triangles on its own; the quality level's detail setting moves those distances too. Set it per terrain with **Detail** in the Inspector.
