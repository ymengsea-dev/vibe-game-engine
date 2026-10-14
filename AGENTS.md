# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust 2024 Cargo workspace. Core libraries live in `crates/engine_*`; `crates/engine` is the public facade and should re-export commonly used subsystem types through its prelude. Executables live in `editor/` and `apps/player/`. The sample project is under `games/island/`, with scenes and imported assets in its `scenes/` and `assets/` directories. Shader files sit beside their owning renderer or editor code. Keep tests close to implementation in `#[cfg(test)] mod tests` blocks.

## Build, Test, and Development Commands

Run commands from the repository root (`vge/`):

```sh
cargo build --workspace --all-features     # Compile the full workspace
cargo run -p editor                        # Launch the editor
cargo test --workspace --all-features      # Run unit tests and doctests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check                 # Verify formatting
cargo deny check                           # Audit licenses and dependencies
```

Use `cargo test -p engine_core test_name` for a focused test. CI also builds documentation and checks Windows and Linux compatibility.

## Coding Style & Naming Conventions

Use rustfmt defaults (four-space indentation) and standard Rust naming: `snake_case` for modules and functions, `CamelCase` for types and traits, and `SCREAMING_SNAKE_CASE` for constants. Production code must not use `unsafe`, `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!`; return typed `Result` errors, normally using `thiserror`. Declare shared third-party versions in the workspace `[workspace.dependencies]` table, then enable them only in crates that need them.

## Testing Guidelines

Add tests with every behavior change, including error and boundary cases. Name tests after the observable result, such as `rejects_missing_scene_asset`. Before opening a PR, run the workspace test, Clippy, and formatting commands above. Changes affecting platform code should also be checked on each supported OS when practical.

## Commit & Pull Request Guidelines

History follows Conventional Commit-style subjects, for example `feat(editor): restore project session`. Use an imperative, scoped subject and keep each commit focused. PRs should explain the user-visible change, identify affected crates, link relevant issues or tasks, and list verification commands. Include screenshots or a short recording for editor UI changes and call out asset, scene-format, or migration impacts.

## Security & Repository Hygiene

Never commit credentials, provider keys, generated `target/` output, or local editor state. Keep runtime crates independent of editor and AI-only code. Treat asset paths and project-root file operations as untrusted input and preserve existing validation and permission gates.

## Production Engineering Rules

### Requirements

- Understand the existing implementation before changing it.
- Identify acceptance criteria and relevant edge cases before implementing non-trivial features.
- Ask for clarification when an unresolved requirement materially affects correctness, security, or architecture.
- Do not invent business rules or silently change existing behavior.

### Implementation

- Prefer simple, maintainable solutions over unnecessary abstractions.
- Preserve existing architecture unless there is a concrete reason to change it.
- Keep changes focused on the requested feature or bug.
- Validate untrusted input at system boundaries.
- Handle expected failures explicitly; never silently swallow errors.
- Do not hardcode secrets or expose sensitive information in logs.
- Do not introduce dependencies without explaining their purpose.

### Testing

- For bug fixes, reproduce the bug with a regression test when practical.
- For new behavior, test expected outcomes, invalid inputs, boundary cases, and relevant failure paths.
- Prefer integration tests for behavior involving databases, authentication, HTTP requests, or multiple components.
- Run the relevant tests and project quality checks after changes.
- Never change or remove a test merely to make the suite pass without establishing that the test itself is incorrect.

### Verification

- Report the exact checks executed and their results.
- Distinguish tests that passed from tests that were not run.
- Never claim that behavior works solely because the code appears correct.
- Review the final diff for unrelated changes, regressions, security issues, and missing error handling.

### Data and Operations

- Treat database migrations, authentication, authorization, and destructive operations as high-risk changes.
- Never run destructive production operations without explicit approval.
- For schema changes, consider existing data, migration compatibility, rollback, and deployment order.
- Never commit secrets, credentials, or real user data.

### Completion Criteria

A task is complete only when its acceptance criteria are satisfied, relevant checks have been executed, and remaining risks or limitations are documented.
