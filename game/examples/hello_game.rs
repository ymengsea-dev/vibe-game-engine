//! The smallest useful game on `engine::app`: a spinning cube on a ground
//! plane with an orbiting camera. Escape quits.
//!
//! Run with `cargo run -p game --example hello_game`. Compare its ~60
//! lines to `game/src/main.rs` — the window, GPU context, PBR/shadow/
//! skybox/HDR/post pipelines, ECS world, and frame loop are all owned by
//! `run_game`.

use engine::prelude::*;

struct HelloGame {
    cube: Option<Entity>,
    angle: f32,
}

impl Game for HelloGame {
    fn setup(&mut self, ctx: &mut GameContext<'_>) -> Result<(), GameError> {
        let white = ctx
            .gpu()
            .create_texture_from_rgba("white", 1, 1, &[255, 255, 255, 255]);

        let (vertices, indices) = cube();
        let cube_mesh = ctx
            .gpu()
            .create_mesh("cube", &vertices, &indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        self.cube = Some(ctx.spawn_mesh(
            cube_mesh,
            &white,
            Material {
                base_color_factor: [0.9, 0.5, 0.3, 1.0],
                metallic_factor: 0.1,
                roughness_factor: 0.6,
            },
            Transform::from_translation(glam::Vec3::new(0.0, 0.6, 0.0)),
        ));

        let ground_mesh = ctx
            .gpu()
            .create_mesh("ground", &vertices, &indices)
            .map_err(|err| GameError::Setup(err.to_string()))?;
        ctx.spawn_mesh(
            ground_mesh,
            &white,
            Material {
                base_color_factor: [0.4, 0.6, 0.4, 1.0],
                metallic_factor: 0.0,
                roughness_factor: 0.9,
            },
            Transform::from_translation(glam::Vec3::new(0.0, -0.25, 0.0))
                .with_scale(glam::Vec3::new(8.0, 0.5, 8.0)),
        );

        ctx.lights_mut().point.push(PointLight {
            position: glam::Vec3::new(2.0, 3.0, 2.0),
            color: glam::Vec3::new(1.0, 0.9, 0.7),
            intensity: 12.0,
            range: 20.0,
        });

        Ok(())
    }

    fn update(&mut self, ctx: &mut GameContext<'_>, dt: f32) {
        if ctx.input().is_key_held(KeyCode::Escape) {
            ctx.request_exit();
            return;
        }

        self.angle += dt * 0.6;

        if let Some(cube) = self.cube
            && let Some(mut transform) = ctx
                .world_mut()
                .get_mut::<engine::ecs::components::Transform>(cube)
        {
            transform.0.rotation = glam::Quat::from_rotation_y(self.angle * 2.0);
        }

        let radius = 6.0;
        let camera = ctx.camera_mut();
        camera.eye = glam::Vec3::new(self.angle.cos() * radius, 3.0, self.angle.sin() * radius);
        camera.target = glam::Vec3::new(0.0, 0.5, 0.0);
    }
}

fn main() -> Result<(), GameError> {
    let _ = logging::init_default();
    run_game(
        GameConfig::new("Hello VGE", 1280, 720),
        HelloGame {
            cube: None,
            angle: 0.0,
        },
    )
}
