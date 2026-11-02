# RustyEngine Studio Developer Documentation

Documentation for people building Studio and the engine.

| Document | Purpose |
| --- | --- |
| [01_STUDIO_FEATURES.md](01_STUDIO_FEATURES.md) | Every Studio feature in plain language, with its status (✅ done, 🟡 partly done, ⬜ not started) |
| [02_ENGINE_FEATURES.md](02_ENGINE_FEATURES.md) | Code-verified inventory of the engine runtime (renderer, ECS, physics, audio and more), as of 2026-10-13 |
| [03_UI_PROTOTYPE.html](03_UI_PROTOTYPE.html) | Early HTML prototype of the editor layout (panel arrangement only; the look is now defined by 04_UI_STYLE.md) |
| [04_UI_STYLE.md](04_UI_STYLE.md) | UI style rules: palette, type, shapes, shared controls, writing, and how to check a UI change |

Design specs and implementation plans live in `.superpowers/` at the repository root (managed by the superpowers skill, not versioned).

## Keeping documentation current

Documentation changes ship in the same change as the code they describe.

| When a change… | Update |
| --- | --- |
| Adds or changes a feature, or changes its status | [01_STUDIO_FEATURES.md](01_STUDIO_FEATURES.md) |
| Adds, removes or changes a shortcut or mouse action | [user/01_KEYBOARD_SHORTCUTS.md](../user/01_KEYBOARD_SHORTCUTS.md) |
| Adds or changes a Scene View option, overlay, marker or menu item | [user/02_SCENE_VIEW_GUIDE.md](../user/02_SCENE_VIEW_GUIDE.md) |
| Adds a new user-facing area of the editor | A new guide in `docs/user/`, listed in [user/00_README.md](../user/00_README.md) |
| Adds or changes a style token, shared control or style rule | [04_UI_STYLE.md](04_UI_STYLE.md) |

Rules for user documentation:

- Write for game makers, not engineers: describe what an option does and when to use it. Leave out code paths and internal names.
- Verify every statement against the code before writing it.
- State limitations plainly (for example, a "Not available yet" section) rather than leaving them out.
