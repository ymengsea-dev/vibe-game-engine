//! The Scenes window: every scene file in the project, which one the game
//! starts on, and the small dialog that names a new scene.

use std::path::{Path, PathBuf};

use crate::dirty::PendingAction;
use crate::state::{EditorState, SceneNameDialog, SceneNameKind, scene_display_name};

/// Draws the Scenes window and the naming dialog while either is open.
pub(crate) fn show(ctx: &egui::Context, state: &mut EditorState) {
    if state.scenes_window_open {
        window(ctx, state);
    }
    if state.scene_name_dialog.is_some() {
        name_dialog(ctx, state);
    }
}

/// What one row of the window asked for.
enum RowAction {
    Open(PathBuf),
    OpenBeside(PathBuf),
    Close(String),
    SetStart(PathBuf),
    Move(PathBuf, i32),
}

/// How one row of the window stands.
#[derive(Clone, Copy)]
struct RowState {
    is_main: bool,
    is_beside: bool,
    is_start: bool,
    first: bool,
    last: bool,
}

fn window(ctx: &egui::Context, state: &mut EditorState) {
    let palette = crate::theme::NIGHT;
    let scenes = state.scene_list().to_vec();
    let current = state.current_scene();
    let start = state.project.manifest().main_scene.clone();
    let beside: Vec<String> = state.extra_scenes().map(str::to_owned).collect();
    let has_loading_screen = state.has_loading_screen();
    let error = state.scenes_error.clone();
    let mut new_loading_screen = false;
    let mut action = None;
    let mut close = false;
    let mut new_scene = false;
    let mut save_as = false;
    let response = crate::theme::modal(ctx, egui::Id::new("studio_scenes_modal"), |ui| {
        ui.set_width(600.0);
        ui.heading("Scenes");
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("A scene is one level or screen of your game. Going to the next scene follows this order.")
                .size(12.0)
                .color(palette.subtle),
        );
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                for (index, scene) in scenes.iter().enumerate() {
                    let row = RowState {
                        is_main: current.as_deref() == Some(scene),
                        is_beside: beside.iter().any(|file| Path::new(file) == scene),
                        is_start: *scene == start,
                        first: index == 0,
                        last: index + 1 == scenes.len(),
                    };
                    if let Some(picked) = scene_row(ui, scene, row) {
                        action = Some(picked);
                    }
                }
            });
        if let Some(error) = &error {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(error).size(12.0).color(palette.danger));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            new_scene = ui.button("New Scene…").clicked();
            save_as = ui.button("Save Scene As…").clicked();
            if !has_loading_screen {
                new_loading_screen = ui
                    .button("New Loading Screen")
                    .on_hover_text("A scene whose Game UI shows while another scene loads")
                    .clicked();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button("Close").clicked();
            });
        });
    });
    if close || response.should_close() {
        state.scenes_window_open = false;
        state.scenes_error = None;
    }
    if new_loading_screen {
        state.scenes_window_open = false;
        crate::shell::request_guarded(state, PendingAction::CreateLoadingScreen);
    }
    if new_scene {
        state.scene_name_dialog = Some(SceneNameDialog::new(SceneNameKind::New));
    }
    if save_as {
        state.scene_name_dialog = Some(SceneNameDialog::new(SceneNameKind::SaveAs));
    }
    match action {
        Some(RowAction::Open(scene)) => {
            state.scenes_error = state.make_working_scene(&scene).err();
            state.scenes_window_open = state.scenes_error.is_some();
        }
        Some(RowAction::OpenBeside(scene)) => {
            state.scenes_error = state.open_scene_beside(&scene).err();
        }
        Some(RowAction::Close(file)) => {
            crate::shell::request_guarded(state, PendingAction::CloseScene(file));
        }
        Some(RowAction::SetStart(scene)) => {
            state.scenes_error = state.set_start_scene(&scene).err();
        }
        Some(RowAction::Move(scene, steps)) => {
            state.scenes_error = state.move_scene_in_order(&scene, steps).err();
        }
        None => {}
    }
}

fn scene_row(ui: &mut egui::Ui, scene: &Path, row: RowState) -> Option<RowAction> {
    let palette = crate::theme::NIGHT;
    let mut action = None;
    let open = row.is_main || row.is_beside;
    let is_loading_screen = scene == Path::new(engine_project::LOADING_SCENE);
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        if ui
            .add_enabled(!row.first, egui::Button::new("↑").small())
            .on_hover_text("Earlier in the scene order")
            .clicked()
        {
            action = Some(RowAction::Move(scene.to_path_buf(), -1));
        }
        if ui
            .add_enabled(!row.last, egui::Button::new("↓").small())
            .on_hover_text("Later in the scene order")
            .clicked()
        {
            action = Some(RowAction::Move(scene.to_path_buf(), 1));
        }
        let name = egui::RichText::new(scene_display_name(scene)).color(if open {
            palette.text
        } else {
            palette.muted
        });
        if ui
            .selectable_label(open, name)
            .on_hover_text(if row.is_main {
                "You are working on this scene"
            } else {
                "Work on this scene"
            })
            .clicked()
            && !row.is_main
        {
            action = Some(RowAction::Open(scene.to_path_buf()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if is_loading_screen {
                note(ui, "Shows while a scene loads", palette.accent);
            } else if row.is_start {
                note(ui, "Game starts here", palette.accent);
            } else if ui
                .button("Set as start")
                .on_hover_text("Make the game start on this scene")
                .clicked()
            {
                action = Some(RowAction::SetStart(scene.to_path_buf()));
            }
            if row.is_beside {
                if ui.button("Close").clicked() {
                    action = Some(RowAction::Close(scene_key(scene)));
                }
            } else if !row.is_main
                && ui
                    .button("Open beside")
                    .on_hover_text("List its objects in the Hierarchy too")
                    .clicked()
            {
                action = Some(RowAction::OpenBeside(scene.to_path_buf()));
            }
        });
    });
    action
}

fn note(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.label(egui::RichText::new(text).size(11.5).color(color));
}

fn scene_key(scene: &Path) -> String {
    scene.to_string_lossy().replace('\\', "/")
}

fn name_dialog(ctx: &egui::Context, state: &mut EditorState) {
    let Some(mut dialog) = state.scene_name_dialog.take() else {
        return;
    };
    let mut confirm = false;
    let mut cancel = false;
    let response = crate::theme::modal(ctx, egui::Id::new("studio_scene_name_modal"), |ui| {
        ui.set_width(320.0);
        ui.heading(match &dialog.kind {
            SceneNameKind::New => "New Scene",
            SceneNameKind::SaveAs => "Save Scene As",
            SceneNameKind::Rename(_) => "Rename Scene",
            SceneNameKind::NewGroup(_) => "New Group",
            SceneNameKind::RenameGroup(_) => "Rename Group",
        });
        ui.add_space(6.0);
        let field = ui.add(
            egui::TextEdit::singleline(&mut dialog.name)
                .hint_text("Level 2")
                .desired_width(f32::INFINITY),
        );
        if !dialog.focused {
            field.request_focus();
            dialog.focused = true;
        }
        if field.changed() {
            dialog.error = None;
        }
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            confirm = true;
        }
        if let Some(error) = &dialog.error {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(error)
                    .size(12.0)
                    .color(crate::theme::NIGHT.danger),
            );
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            confirm |= ui
                .add_enabled(
                    !dialog.name.trim().is_empty(),
                    egui::Button::new(match &dialog.kind {
                        SceneNameKind::New | SceneNameKind::NewGroup(_) => "Create",
                        SceneNameKind::SaveAs => "Save",
                        SceneNameKind::Rename(_) | SceneNameKind::RenameGroup(_) => "Rename",
                    }),
                )
                .clicked();
            cancel = ui.button("Cancel").clicked();
        });
    });
    if cancel || response.should_close() {
        return;
    }
    if !confirm {
        state.scene_name_dialog = Some(dialog);
        return;
    }
    let name = dialog.name.clone();
    let outcome = match &dialog.kind {
        SceneNameKind::New => state.create_working_scene(&name).map(|()| Vec::new()),
        SceneNameKind::SaveAs => state.check_new_scene_name(&name).and_then(|()| {
            state
                .save_scene_as(&name)
                .map(|()| Vec::new())
                .map_err(|err| err.to_string())
        }),
        SceneNameKind::Rename(scene) => state.rename_scene(scene, &name),
        SceneNameKind::NewGroup(scene) => state.create_scene_group(&name).and_then(|group| {
            scene.as_ref().map_or(Ok(Vec::new()), |scene| {
                state.move_scene_to_group(scene, Some(&group))
            })
        }),
        SceneNameKind::RenameGroup(group) => state.rename_scene_group(group, &name),
    };
    match outcome {
        Ok(scripts) => {
            state.scenes_window_open = false;
            crate::shell::show_scripts_left_behind(state, &scripts);
        }
        Err(message) => {
            dialog.error = Some(message);
            state.scene_name_dialog = Some(dialog);
        }
    }
}
