//! Your game. The scene you build in Studio runs through `GameplayRuntime`,
//! the same engine runtime Lua games use: visual rules, UI, collisions,
//! spawning and scene changes all work here too. Add your gameplay in the
//! hooks marked "Your gameplay".

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use engine::prelude::*;

struct MyGame {
    gameplay: GameplayRuntime,
}

impl MyGame {
    /// Your gameplay: runs every frame. `dt` is the frame time in seconds.
    ///
    /// Examples:
    /// - Move an object: `self.gameplay.apply(ctx, vec![GameCommand::Translate { object_id, delta: [2.0 * dt, 0.0, 0.0] }])`
    /// - Run visual rules for an event: `self.gameplay.send_event(ctx, "game_start");`
    /// - Switch level next frame: `self.gameplay.request_scene_change("scenes/level2.ron");`
    fn on_update(&mut self, _ctx: &mut GameContext<'_>, _dt: f32) {}

    /// Your gameplay: an authored UI button was clicked (its visual rules already ran).
    fn on_ui_click(&mut self, _ctx: &mut GameContext<'_>, _button_id: &str) {}

    /// Your gameplay: two objects with colliders started or stopped touching.
    fn on_collision(&mut self, _ctx: &mut GameContext<'_>, _contact: &CollisionEvent) {}
}

impl Game for MyGame {
    fn frame_demand(&self, now: Instant) -> FrameDemand {
        FrameDemand::AnimateUntil(now + Duration::from_millis(16))
    }

    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        self.gameplay.setup(ctx)
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        self.gameplay.begin_frame(ctx);
        for button_id in self.gameplay.dispatch_ui_clicks(ctx) {
            self.on_ui_click(ctx, &button_id);
        }
        self.on_update(ctx, dt);
        if ctx.input().is_key_pressed(KeyCode::Escape) {
            ctx.request_exit();
        }
    }

    fn post_fixed_update(&mut self, ctx: &mut GameContext<'_>, _dt: f32) {
        for contact in self.gameplay.sample_collisions(ctx) {
            self.on_collision(ctx, &contact);
        }
    }
}

/// Loads the exported game beside the executable, or the project when run from Studio.
fn load_game() -> Result<MyGame, Box<dyn std::error::Error>> {
    let executable = executable_dir().ok_or("could not locate the game executable")?;
    let (scene_path, assets, source) = if executable.join(EXPORT_SCENE_FILE).is_file() {
        let assets = open_export(&executable)
            .transpose()?
            .map(|export| export.library);
        (
            executable.join(EXPORT_SCENE_FILE),
            assets,
            SceneSource::Export(executable),
        )
    } else {
        let project = Project::open(std::env::current_dir()?)?;
        let scene_path = match std::env::var_os("VGE_SCENE").map(PathBuf::from) {
            Some(path) if path.is_absolute() => path,
            Some(path) => project.resolve(&path)?,
            None => project.main_scene_path(),
        };
        let assets = AssetLibrary::index(&project.assets_dir())?;
        (scene_path, Some(assets), SceneSource::Project(project))
    };
    let mut scene = Scene::load_from_file(Path::new(&scene_path))?;
    scene.validate()?;
    scene.assign_missing_object_ids();
    Ok(MyGame {
        gameplay: GameplayRuntime::new(scene, assets).with_scene_source(source),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let game = load_game()?;
    run_game(GameConfig::new("My Game", 1280, 720), game)?;
    Ok(())
}
