//! Gameplay commands shared by every game, plus bounded Lua gameplay callbacks
//! behind the `lua` feature.

mod command;
#[cfg(feature = "lua")]
mod lua;

pub use command::GameCommand;

#[cfg(feature = "lua")]
pub use lua::{
    LUA_DIAGNOSTIC_PREFIX, LuaRuntime, LuaScriptDiagnostic, LuaSyntaxDiagnostic, ScriptError,
    ScriptKey, check_lua_syntax,
};

#[cfg(all(test, feature = "lua"))]
mod tests {
    use super::*;

    #[test]
    fn lua_can_change_authored_ui_text_but_rejects_oversized_text() {
        let id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!(
                    "return {{ start = function() engine.ui_set_text('{id}', 'Score: 1') end }}"
                ),
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::UiSetText {
                object_id: id.into(),
                text: "Score: 1".into(),
            }]
        );

        let mut oversized = LuaRuntime::new().expect("runtime");
        oversized
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.ui_set_text('67d09e47-5520-4b5a-a132-a68a4a60f452', string.rep('x', 1025)) end }",
            )
            .expect("script");
        assert!(oversized.start().is_err());
    }

    #[test]
    fn syntax_check_reports_line_without_running_the_script() {
        assert!(check_lua_syntax("assets/scripts/main.lua", "error('must not run')").is_ok());
        let error = check_lua_syntax(
            "assets/scripts/main.lua",
            "return {\n  update = function(]\n}",
        )
        .expect_err("invalid Lua must be diagnosed");
        assert_eq!(error.line, 2);
        assert!(!error.message.is_empty());
    }

    #[test]
    fn callback_error_reports_its_script_and_source_line() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { update = function()\n  error('broken rule')\nend }",
            )
            .expect("script");
        let error = runtime.update(0.1).expect_err("callback fails");
        let diagnostic = error.diagnostic().expect("source diagnostic");
        assert_eq!(diagnostic.path, "assets/scripts/main.lua");
        assert_eq!(diagnostic.line, 2);
        assert!(diagnostic.message.contains("broken rule"));
    }

    #[test]
    fn syntax_error_reports_its_script_and_source_line() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let error = runtime
            .load_script(
                "assets/scripts/actor.lua",
                "return {\n  update = function(]\n}",
            )
            .expect_err("syntax error");
        let diagnostic = error.diagnostic().expect("source diagnostic");
        assert_eq!(diagnostic.path, "assets/scripts/actor.lua");
        assert_eq!(diagnostic.line, 2);
    }

    #[test]
    fn lua_callbacks_run_in_order_and_report_logs() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.log('started') end, update = function(dt) engine.log('dt=' .. dt) end }",
            )
            .expect("script loads");

        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("started".into())]
        );
        assert_eq!(
            runtime.update(0.5).expect("update"),
            vec![GameCommand::Log("dt=0.5".into())]
        );
    }

    #[test]
    fn lua_play_audio_queues_a_scene_emitter_and_rejects_non_object_ids() {
        let id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() engine.play_audio('{id}') end }}"),
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::PlayAudio {
                object_id: id.into()
            }]
        );

        let mut invalid = LuaRuntime::new().expect("runtime");
        invalid
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.play_audio('../outside.wav') end }",
            )
            .expect("script");
        assert!(
            invalid
                .start()
                .expect_err("not an object ID")
                .to_string()
                .contains("object ID")
        );
    }

    #[test]
    fn lua_can_pause_an_authored_animation() {
        let mut runtime = LuaRuntime::new().unwrap();
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.set_animation_playing('67d09e47-5520-4b5a-a132-a68a4a60f452', false) end }",
            )
            .unwrap();
        assert_eq!(
            runtime.start().unwrap(),
            vec![GameCommand::SetAnimationPlaying {
                object_id: "67d09e47-5520-4b5a-a132-a68a4a60f452".into(),
                playing: false,
            }]
        );
    }

    #[test]
    fn lua_can_emit_a_named_visual_rule_event() {
        let mut runtime = LuaRuntime::new().unwrap();
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.emit_event('start_game') end }",
            )
            .unwrap();
        assert_eq!(
            runtime.start().unwrap(),
            vec![GameCommand::EmitEvent {
                name: "start_game".into(),
            }]
        );
    }

    #[test]
    fn lua_rejects_invalid_visual_rule_event_names() {
        for name in [
            "",
            "bad\nname",
            "a_very_long_event_name_that_exceeds_the_sixty_four_byte_limit_for_visual_rules",
        ] {
            let mut runtime = LuaRuntime::new().unwrap();
            runtime
                .load_script(
                    "assets/scripts/main.lua",
                    &format!("return {{ start = function() engine.emit_event({name:?}) end }}"),
                )
                .unwrap();
            assert!(runtime.start().is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn lua_limits_audio_triggers_per_dispatch() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() for i = 1, 33 do engine.play_audio('67d09e47-5520-4b5a-a132-a68a4a60f452') end end }",
            )
            .expect("script");
        assert!(
            runtime
                .start()
                .expect_err("audio flood")
                .to_string()
                .contains("audio play limit")
        );
    }

    #[test]
    fn lua_scene_change_queues_only_a_project_scene() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.change_scene('scenes/level2.ron') end }",
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("scene request"),
            vec![GameCommand::ChangeScene {
                path: "scenes/level2.ron".into()
            }]
        );

        let mut invalid = LuaRuntime::new().expect("runtime");
        invalid
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.change_scene('../other.ron') end }",
            )
            .expect("script");
        assert!(
            invalid
                .start()
                .expect_err("traversal is rejected")
                .to_string()
                .contains("scene path")
        );
        let mut platform_separator = LuaRuntime::new().expect("runtime");
        platform_separator
            .load_script(
                "assets/scripts/main.lua",
                r"return { start = function() engine.change_scene([[scenes\level2.ron]]) end }",
            )
            .expect("script");
        assert!(
            platform_separator
                .start()
                .expect_err("backslash path is rejected")
                .to_string()
                .contains("scene path")
        );
        let mut repeated_separator = LuaRuntime::new().expect("runtime");
        repeated_separator
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.change_scene('scenes//level2.ron') end }",
            )
            .expect("script");
        assert!(repeated_separator.start().is_err());
    }

    #[test]
    fn first_scene_request_wins_when_multiple_scripts_request_a_change() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.change_scene('scenes/first.ron') end }",
            )
            .expect("global script");
        runtime
            .load_attached_script(
                "assets/scripts/actor.lua",
                "return { start = function() engine.change_scene('../invalid.ron') end }",
                "67d09e47-5520-4b5a-a132-a68a4a60f452",
            )
            .expect("attached script");
        assert_eq!(
            runtime.start().expect("first request is retained"),
            vec![GameCommand::ChangeScene {
                path: "scenes/first.ron".into()
            }]
        );
    }

    #[test]
    fn lua_save_values_restore_before_start_and_request_a_write() {
        let mut first = LuaRuntime::new().expect("runtime");
        first
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.save_set('score', 3); engine.save_set('won', true); engine.save_set('name', 'Ada'); engine.save() end }",
            )
            .expect("script");
        assert_eq!(
            first.start().expect("start"),
            vec![GameCommand::RequestSave]
        );
        let saved = first.saved_data().expect("serialize");
        let mut reopened = LuaRuntime::new().expect("runtime");
        reopened
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.log(tostring(engine.save_get('score')) .. ',' .. tostring(engine.save_get('won')) .. ',' .. engine.save_get('name')) end }",
            )
            .expect("script");
        reopened.restore_saved_data(&saved).expect("restore");
        assert_eq!(
            reopened.start().expect("start"),
            vec![GameCommand::Log("3,true,Ada".into())]
        );
    }

    #[test]
    fn lua_save_rejects_unsupported_or_oversized_data_without_changing_progress() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.save_set('score', 7) end, update = function() engine.save_set('bad', {}); engine.save() end }",
            )
            .expect("script");
        runtime.start().expect("start");
        let saved = runtime.saved_data().expect("saved progress");
        assert!(runtime.update(0.1).is_err());
        assert_eq!(runtime.saved_data().expect("unchanged progress"), saved);
        assert!(runtime.restore_saved_data(&"x".repeat(70_000)).is_err());
        assert_eq!(runtime.saved_data().expect("still unchanged"), saved);
    }

    #[test]
    fn lua_save_write_flood_stops_before_it_can_stall_a_frame() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { update = function() for i = 1, 100000 do engine.save_set('score', i) end end }",
            )
            .expect("script");
        let error = runtime.update(0.1).expect_err("save write budget");
        assert!(error.to_string().contains("save write limit"));
        assert_eq!(runtime.saved_data().expect("rolled back"), "{}");
    }

    #[test]
    fn lua_instruction_limit_applies_across_all_attached_scripts() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let source =
            "return { start = function() local n = 0; for i = 1, 400000 do n = n + i end end }";
        let mut single = LuaRuntime::new().expect("runtime");
        single
            .load_attached_script("assets/scripts/actor.lua", source, "only")
            .expect("script");
        single.start().expect("one script fits the budget");
        for id in ["first", "second"] {
            runtime
                .load_attached_script("assets/scripts/actor.lua", source, id)
                .expect("script");
        }
        let error = runtime.start().expect_err("combined callback budget");
        assert!(error.to_string().contains("instruction limit"));
    }

    #[test]
    fn lua_rejects_unsafe_libraries() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.log(type(io) .. ',' .. type(os) .. ',' .. type(package) .. ',' .. type(debug) .. ',' .. type(dofile) .. ',' .. type(loadfile)) end }",
            )
            .expect("script loads");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("nil,nil,nil,nil,nil,nil".into())]
        );
    }

    #[test]
    fn lua_infinite_loop_returns_an_error() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() while true do end end }",
            )
            .expect("script loads");
        let error = runtime.start().expect_err("loop must stop");
        assert!(error.to_string().contains("instruction limit"));
    }

    #[test]
    fn lua_protected_calls_cannot_swallow_the_instruction_limit() {
        for source in [
            "return { start = function() while true do pcall(function() while true do end end) end end }",
            "return { start = function() while true do xpcall(function() while true do end end, function(e) return e end) end end }",
            "return { start = function() while true do pcall(pcall, function() while true do end end) end end }",
        ] {
            let mut runtime = LuaRuntime::new().expect("runtime");
            runtime
                .load_script("assets/scripts/main.lua", source)
                .expect("script loads");
            let error = runtime.start().expect_err("protected loop must stop");
            assert!(error.to_string().contains("instruction limit"), "{error}");
        }
    }

    #[test]
    fn lua_pcall_still_catches_ordinary_errors() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() local ok, err = pcall(error, 'boom') engine.log(tostring(ok) .. ':' .. tostring(err)) end }",
            )
            .expect("script loads");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("false:boom".into())]
        );
    }

    #[test]
    fn lua_budget_resets_after_an_exhausted_callback() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() while true do pcall(function() while true do end end) end end, update = function() local ok = pcall(function() end) engine.log(tostring(ok)) end }",
            )
            .expect("script loads");
        runtime.start().expect_err("start exhausts its budget");
        assert_eq!(
            runtime
                .update(0.016)
                .expect("next callback has a fresh budget"),
            vec![GameCommand::Log("true".into())]
        );
    }

    #[test]
    fn lua_rejects_scripts_outside_asset_scripts() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        assert!(runtime.load_script("../outside.lua", "return {}").is_err());
        assert!(
            runtime
                .load_script("assets/scripts/main.rs", "return {}")
                .is_err()
        );
    }

    #[test]
    fn lua_log_queue_is_bounded() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() for i = 1, 2000 do engine.log('x') end end }",
            )
            .expect("script loads");
        let error = runtime.start().expect_err("logs must be bounded");
        assert!(error.to_string().contains("log limit"));
    }

    #[test]
    fn attached_scripts_get_their_own_object_identity_in_load_order() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let source = "return { start = function() engine.log(engine.object_id()) end }";
        runtime
            .load_attached_script("assets/scripts/actor.lua", source, "first-object")
            .expect("first script");
        runtime
            .load_attached_script("assets/scripts/actor.lua", source, "second-object")
            .expect("second script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![
                GameCommand::Log("first-object".into()),
                GameCommand::Log("second-object".into()),
            ]
        );
    }

    #[test]
    fn lua_transform_request_is_bounded_and_rejects_nonfinite_positions() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_attached_script(
                "assets/scripts/actor.lua",
                "return { update = function(dt) engine.set_position(engine.object_id(), dt, 2, 3) end }",
                "actor-id",
            )
            .expect("script");
        assert_eq!(
            runtime.update(1.5).expect("update"),
            vec![GameCommand::SetPosition {
                object_id: "actor-id".into(),
                position: [1.5, 2.0, 3.0],
            }]
        );

        let mut invalid = LuaRuntime::new().expect("runtime");
        invalid
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.set_position('actor-id', math.huge, 0, 0) end }",
            )
            .expect("script");
        assert!(
            invalid
                .start()
                .expect_err("invalid position")
                .to_string()
                .contains("finite")
        );
    }

    #[test]
    fn lua_translate_requests_parent_relative_movement() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_attached_script(
                "assets/scripts/actor.lua",
                "return { update = function(dt) if engine.key_down('D') then engine.translate(engine.object_id(), 3 * dt, 0, 0) end end }",
                "actor-id",
            )
            .expect("script");
        runtime.set_key_state(ScriptKey::D, true, true);
        assert_eq!(
            runtime.update(0.5).expect("update"),
            vec![GameCommand::Translate {
                object_id: "actor-id".into(),
                delta: [1.5, 0.0, 0.0],
            }]
        );
    }

    #[test]
    fn lua_reads_current_key_state_and_rejects_unknown_names() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { update = function() engine.log(tostring(engine.key_down('A')) .. ',' .. tostring(engine.key_pressed('Space'))) end }",
            )
            .expect("script");
        runtime.set_key_state(ScriptKey::A, true, false);
        runtime.set_key_state(ScriptKey::Space, true, true);
        assert_eq!(
            runtime.update(0.1).expect("update"),
            vec![GameCommand::Log("true,true".into())]
        );
        runtime.set_key_state(ScriptKey::A, false, false);
        runtime.set_key_state(ScriptKey::Space, true, false);
        assert_eq!(
            runtime.update(0.1).expect("update"),
            vec![GameCommand::Log("false,false".into())]
        );

        let mut bad = LuaRuntime::new().expect("runtime");
        bad.load_script(
            "assets/scripts/main.lua",
            "return { start = function() engine.key_down('Typo') end }",
        )
        .expect("script");
        assert!(
            bad.start()
                .expect_err("unknown key")
                .to_string()
                .contains("unknown key")
        );
    }

    #[test]
    fn lua_event_callbacks_run_in_script_order() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { on_event = function(kind, key) engine.log('global:' .. kind .. ':' .. key) end }",
            )
            .expect("global");
        runtime
            .load_attached_script(
                "assets/scripts/actor.lua",
                "return { on_event = function(kind, key) engine.log(engine.object_id() .. ':' .. kind .. ':' .. key) end }",
                "actor",
            )
            .expect("actor");
        assert_eq!(
            runtime.on_event("key_pressed", "A").expect("event"),
            vec![
                GameCommand::Log("global:key_pressed:A".into()),
                GameCommand::Log("actor:key_pressed:A".into()),
            ]
        );
    }

    #[test]
    fn collision_callback_reaches_global_and_only_the_two_attached_objects() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { on_collision = function(kind, first, second) engine.log('global:' .. kind .. ':' .. first .. ':' .. second) end }",
            )
            .expect("global");
        let attached = "return { on_collision = function(kind, self_id, other_id) engine.log(engine.object_id() .. ':' .. kind .. ':' .. self_id .. ':' .. other_id) end }";
        for id in ["a", "b", "unrelated"] {
            runtime
                .load_attached_script("assets/scripts/actor.lua", attached, id)
                .expect("attached");
        }
        assert_eq!(
            runtime.on_collision("enter", "a", "b").expect("collision"),
            vec![
                GameCommand::Log("global:enter:a:b".into()),
                GameCommand::Log("a:enter:a:b".into()),
                GameCommand::Log("b:enter:b:a".into()),
            ]
        );
    }

    #[test]
    fn missing_scene_object_queries_return_nil() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.log(tostring(engine.find_object('Missing')) .. ',' .. tostring(engine.get_position('missing-id'))) end }",
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("nil,nil".into())]
        );
    }

    #[test]
    fn named_scene_object_query_reads_the_first_objects_parent_relative_position() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_snapshot([
            ("first".into(), Some("Door".into()), Some([1.0, 2.0, 3.0])),
            ("second".into(), Some("Door".into()), Some([9.0, 9.0, 9.0])),
        ]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() local id = engine.find_object('Door'); local p = engine.get_position(id); engine.log(tostring(id == 'first' and p.x == 1 and p.y == 2 and p.z == 3)) end }",
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("true".into())]
        );
    }

    #[test]
    fn objects_without_transforms_can_be_found_but_have_no_position() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_snapshot([("anchor".into(), Some("Anchor".into()), None)]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() local id = engine.find_object('Anchor'); engine.log(id .. ':' .. tostring(engine.get_position(id))) end }",
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("anchor:nil".into())]
        );
    }

    #[test]
    fn position_reads_reflect_queued_writes_in_the_same_callback() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_snapshot([("actor".into(), None, Some([1.0, 2.0, 3.0]))]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.set_position('actor', 4, 5, 6); engine.translate('actor', 1, 0, 0); local p = engine.get_position('actor'); engine.log(tostring(p.x == 5 and p.y == 5 and p.z == 6)) end }",
            )
            .expect("script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![
                GameCommand::SetPosition {
                    object_id: "actor".into(),
                    position: [4.0, 5.0, 6.0],
                },
                GameCommand::Translate {
                    object_id: "actor".into(),
                    delta: [1.0, 0.0, 0.0],
                },
                GameCommand::Log("true".into()),
            ]
        );
    }

    #[test]
    fn lua_spawn_empty_returns_an_id_visible_to_later_commands() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() local id = engine.spawn_empty('Marker', 1, 2, 3); local p = engine.get_position(id); engine.log(tostring(engine.find_object('Marker') == id and p.x == 1 and p.y == 2 and p.z == 3)); engine.translate(id, 2, 0, 0) end }",
            )
            .expect("script");
        let commands = runtime.start().expect("start");
        let [
            GameCommand::SpawnEmpty {
                object_id,
                name,
                position,
            },
            GameCommand::Log(result),
            GameCommand::Translate {
                object_id: moved_id,
                delta,
            },
        ] = commands.as_slice()
        else {
            panic!("spawn, lookup, and movement commands expected: {commands:?}");
        };
        assert!(uuid::Uuid::parse_str(object_id).is_ok());
        assert_eq!(name, "Marker");
        assert_eq!(position, &[1.0, 2.0, 3.0]);
        assert_eq!(result, "true");
        assert_eq!(moved_id, object_id);
        assert_eq!(delta, &[2.0, 0.0, 0.0]);
    }

    #[test]
    fn lua_spawn_prefab_returns_an_id_visible_to_later_commands() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() local id = engine.spawn_prefab('assets/prefabs/Crate.prefab', 1, 2, 3); local p = engine.get_position(id); engine.log(tostring(p.x == 1 and p.y == 2 and p.z == 3)) end }",
            )
            .expect("script loads");
        let commands = runtime.start().expect("start");
        assert!(
            matches!(commands[0], GameCommand::SpawnPrefab { ref path, position: [1.0, 2.0, 3.0], .. } if path == "assets/prefabs/Crate.prefab")
        );
        assert_eq!(commands[1], GameCommand::Log("true".into()));
    }

    #[test]
    fn lua_spawn_prefab_rejects_paths_outside_packaged_prefabs() {
        for path in [
            "../secrets.prefab",
            "assets/prefabs/../secrets.prefab",
            "assets/scripts/actor.lua",
            "/tmp/Crate.prefab",
        ] {
            let mut runtime = LuaRuntime::new().expect("runtime");
            runtime
                .load_script(
                    "assets/scripts/main.lua",
                    &format!(
                        "return {{ start = function() engine.spawn_prefab('{path}', 0, 0, 0) end }}"
                    ),
                )
                .expect("script loads");
            assert!(runtime.start().is_err(), "accepted {path}");
        }
    }

    #[test]
    fn top_level_spawn_is_rejected_before_a_callback_runs() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let error = runtime
            .load_script(
                "assets/scripts/main.lua",
                "engine.spawn_empty('Ghost', 0, 0, 0); return {}",
            )
            .expect_err("top-level command must not be silently lost");
        assert!(error.to_string().contains("callback"));
        runtime
            .load_script(
                "assets/scripts/check.lua",
                "return { start = function() engine.log(tostring(engine.find_object('Ghost'))) end }",
            )
            .expect("checking script");
        assert_eq!(
            runtime.start().expect("start"),
            vec![GameCommand::Log("nil".into())]
        );
    }

    #[test]
    fn failed_callback_does_not_leave_an_unapplied_spawn_visible() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.spawn_empty('Ghost', 0, 0, 0); error('stop') end, update = function() engine.log(tostring(engine.find_object('Ghost'))) end }",
            )
            .expect("script");
        assert!(runtime.start().is_err());
        assert_eq!(
            runtime.update(0.1).expect("update"),
            vec![GameCommand::Log("nil".into())]
        );
    }

    #[test]
    fn lua_spawn_rejects_invalid_names_positions_and_command_floods() {
        for (source, expected) in [
            ("engine.spawn_empty(' ', 0, 0, 0)", "name"),
            ("engine.spawn_empty('Marker', math.huge, 0, 0)", "finite"),
            (
                "for i = 1, 1025 do engine.spawn_empty('Marker', 0, 0, 0) end",
                "command limit",
            ),
        ] {
            let mut runtime = LuaRuntime::new().expect("runtime");
            runtime
                .load_script(
                    "assets/scripts/main.lua",
                    &format!("return {{ start = function() {source} end }}"),
                )
                .expect("script");
            let error = runtime.start().expect_err("invalid spawn must fail");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn lua_variables_read_their_own_writes_and_reach_the_host() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.set_var('score', engine.get_var('score') + 5) engine.log(tostring(engine.get_var('score'))) end, update = function() engine.log(tostring(engine.get_var('score'))) end }",
            )
            .expect("script loads");
        runtime.set_variables([("score", 2.0)]);
        assert_eq!(
            runtime.start().expect("start"),
            vec![
                GameCommand::SetVariable {
                    name: "score".into(),
                    value: 7.0
                },
                GameCommand::Log("7".into()),
            ]
        );
        assert_eq!(
            runtime
                .update(0.016)
                .expect("a later callback keeps the write"),
            vec![GameCommand::Log("7".into())]
        );
        runtime.set_variables([("score", 40.0), ("speed", 2.5)]);
        assert_eq!(
            runtime
                .update(0.016)
                .expect("host values replace the snapshot"),
            vec![GameCommand::Log("40".into())]
        );
    }

    #[test]
    fn lua_variable_writes_roll_back_when_the_callback_fails() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime
            .load_script(
                "assets/scripts/main.lua",
                "return { start = function() engine.set_var('lives', 9) error('boom') end, update = function() engine.log(tostring(engine.get_var('lives'))) end }",
            )
            .expect("script loads");
        runtime.start().expect_err("start fails");
        assert_eq!(
            runtime.update(0.016).expect("update"),
            vec![GameCommand::Log("0".into())]
        );
    }

    #[test]
    fn lua_set_var_rejects_bad_arguments_and_top_level_calls() {
        for (body, expected) in [
            ("engine.set_var('', 1)", "variable name"),
            ("engine.set_var('x', 0/0)", "finite"),
            ("engine.set_var('x', 1/0)", "finite"),
            ("engine.set_var(string.rep('x', 65), 1)", "variable name"),
        ] {
            let mut runtime = LuaRuntime::new().expect("runtime");
            runtime
                .load_script(
                    "assets/scripts/main.lua",
                    &format!("return {{ start = function() {body} end }}"),
                )
                .expect("script loads");
            let error = runtime.start().expect_err("invalid variable write");
            assert!(error.to_string().contains(expected), "{body}: {error}");
        }
        let mut runtime = LuaRuntime::new().expect("runtime");
        assert!(
            runtime
                .load_script(
                    "assets/scripts/main.lua",
                    "engine.set_var('x', 1) return {}"
                )
                .is_err()
        );
    }

    #[test]
    fn start_object_runs_only_that_objects_scripts() {
        let mut runtime = LuaRuntime::new().expect("runtime");
        let source = "return { start = function() engine.log(tostring(engine.object_id())) end }";
        runtime
            .load_script("assets/scripts/main.lua", source)
            .expect("global");
        for id in ["first", "second"] {
            runtime
                .load_attached_script("assets/scripts/actor.lua", source, id)
                .expect("attached");
        }
        assert_eq!(
            runtime.start_object("second").expect("start one"),
            vec![GameCommand::Log("second".into())]
        );
        assert!(
            runtime
                .start_object("missing")
                .expect("no match")
                .is_empty()
        );
    }
}
