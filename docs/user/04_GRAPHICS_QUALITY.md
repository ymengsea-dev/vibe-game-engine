# RustyEngine Studio — Graphics Quality and Distance

Far-away objects can use simpler models or not be drawn at all, so big scenes stay fast. One quality setting decides how much detail the whole game shows.

---

## Graphics quality

Open **File → Settings…** (or press **⌘,**) and choose **Project**.

| Level | What changes |
| --- | --- |
| **Low** | Softer shadows, simpler models sooner, objects drawn less far (60%) |
| **Medium** | The default |
| **High** | Sharper shadows, detailed models for longer, objects drawn further (150%) |

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

### Simpler versions in your model file

Put each version in the same glTF file as its own mesh, named with `_LOD` and a number: `Rock_LOD0` (full detail), `Rock_LOD1`, `Rock_LOD2`. All must share the same name before `_LOD`. Studio then uses them instead of generating its own.

## Seeing it in the Scene View

Turn on **Overlays → Distance detail** to see simpler models and draw ranges from the Scene View camera, at the project's quality. Off, every model is drawn in full.

## Not available yet

- Animated characters, foliage and terrain always draw in full detail and at any distance.
- The Scene View's shadows do not follow the quality setting; only the game's do.
