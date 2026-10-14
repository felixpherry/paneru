# Agent Instructions: Paneru macOS Window Manager (Bevy-based)

This document provides project-specific guidance for AI agents contributing to Paneru. It builds upon the core philosophy and technical architecture of the codebase.

## 1. Bevy ECS Architecture & Patterns (Bevy First)

Paneru is built on Bevy and strictly follows Data-Driven Design (ECS). **Always prioritize Bevy ECS rules over conventional Rust patterns**:
*   Avoid traditional Object-Oriented patterns, abstract trait hierarchies, or "god structs" holding internal state machines.
*   Model state as small, queryable Components and compose functionality through Systems and Observers.
*   **Marker Components:** Use markers extensively for filtering and state tracking (e.g., `ActiveWorkspaceMarker`, `FocusedMarker`, `FreshMarker`, `Unmanaged`). Most markers are found in `src/ecs.rs`.
*   **Avoid Unbounded / Constantly Running Systems:** Systems should not run unconditionally on every tick if there is nothing to process.
    *   **Use `Populated<Query<...>>` instead of `Query<...>`** where a system only acts when matching entities exist. Bevy automatically adds a run condition for `Populated`, ensuring the system is not even scheduled if the query is empty.
    *   Pair queries with change filters (`Changed<T>`, `Added<T>`) and explicit run conditions (`run_if(...)`) so systems remain idle when state is static.
*   **Triggers & Observers:** Prefer Bevy's observer pattern for reactive logic. See `src/ecs/triggers.rs` and `src/ecs/workspace.rs` for examples like `SpawnWindowTrigger` and `SendMessageTrigger`.
*   **System Grouping:** Systems are registered in `src/ecs.rs` via `register_systems`. Follow the existing schedule-based organization (`PreUpdate`, `Update`, `PostUpdate`): put each system in one of the phase sets `PreUpdateSet`, `UpdateSet` or `PostUpdateSet`. Order across sets comes from the set chain; a `.after()` that points into another set can form a schedule cycle.
*   **System Params:** Use custom system parameters like `Windows` and `ActiveDisplay` (defined in `src/ecs/params.rs`) to simplify queries.

## 2. macOS & AppKit Integration (The Bridge)

*   **Main Thread Constraint:** All AppKit/CoreGraphics calls MUST happen on the main thread.
*   **Single-Threaded Schedules:** The main-thread guarantee is the `SingleThreadedExecutor` that `build_core_app` (`src/ecs.rs`) sets on every main schedule (`First` through `Last`). Never switch these schedules to the multi-threaded executor.
*   **NonSend Resources:** Use `NonSend` and `NonSendMut` for resources that are not thread-safe (e.g., `OverlayManager`, `PlatformCallbacks`). `WindowManager` is a plain `Resource`; its `WindowManagerApi` trait is `Send + Sync`.
*   **FFI Wrappers:** Interact with macOS via the abstractions in `src/manager/` and `src/platform/`. Avoid direct `objc2` calls in ECS systems; use the `WindowManager` API.
*   **Change Detection:** Use `Changed<T>` to trigger expensive macOS API updates (like window repositioning) only when the ECS state actually changes.
*   **The Lua Worker:** The scripting runtime (`src/lua/worker.rs`, `lua` feature) runs on its own thread — handlers are user code of unbounded duration and must never stall `pump_events`. Anything crossing that boundary must be plain `Send` data, never a Lua value or an ECS borrow; world access from a script goes through the `serve_lua_queries` round-trip. If you add a main-thread-only FFI call to a path a script can reach (`resolve_chord` is the existing example), compute it on the main thread and cache it — see `config::prime_virtual_keymap`.

## 3. Layout & Workspace Logic

*   **LayoutStrip:** The core layout data structure is `LayoutStrip` (in `src/ecs/layout.rs`). It manages columns, stacks, and tabs.
*   **Virtual Workspaces:** Paneru manages virtual workspaces that map to macOS "Spaces". See `src/ecs/workspace.rs` for how window movement and workspace switching are handled.
*   **Coordinate Systems:** Be aware of the difference between Bevy's coordinate system (often Y-up) and macOS/AppKit (Y-down). Use the `Position` and `Size` abstractions to handle conversions.

## 4. Coding Standards & Idioms

*   **Clippy:** Paneru enforces strict Clippy lints. Run `cargo clippy` before finalizing changes.
*   **Formatting:** All code must be formatted using `cargo fmt`.
*   **Tracing:** Use the `tracing` crate for logging. Use `#[instrument(level = Level::DEBUG, skip_all, fields(...))]` for complex systems.
*   **Error Handling:** Use the project's `Result` type and `Error` enum in `src/errors.rs`. Avoid `unwrap()` in systems; log errors or use `inspect_err`.

## 5. Testing Strategy

*   **Mocking:** When adding features that interact with macOS, ensure the logic is separable so it can be tested with a mock `WindowManager`.
*   **Harness-Based Integration Tests:** Unit and integration tests drive an isolated Bevy `World` via `TestHarness`, which builds the same core app as the daemon (`build_core_app`). Tests should adhere to the established harness structure:
    1. Define a list of input events/commands (`vec![Event::..., Event::Command { ... }]`).
    2. Configure the harness (e.g., `TestHarness::new().with_windows(n)` or `.with_config(...)`).
    3. Assert expected world/state outcomes on specific iterations using `.on_iteration(step, |world, state| { ... })`. Steps count from 0: `on_iteration(n, ...)` runs after command n, so the first command is step 0. `run` panics if a check's step is never reached.
    4. Execute via `.run(commands)`.

    **Example** (from `src/tests/focus_or_virtual.rs`; `active_virtual_index` is a helper in that file):
    ```rust
    #[test]
    fn test_focus_or_virtual_south_focuses_stack_sibling_before_switching_workspace() {
        let commands = vec![
            Event::MenuOpened { window_id: 0 },
            Event::Command {
                command: Command::Window(Operation::Focus(Direction::East)),
            },
            Event::Command {
                command: Command::Window(Operation::Stack(true)),
            },
            // Jump to VW1 and straight back so it exists for the eventual
            // fallback switch, without relying on auto-create being enabled.
            Event::Command {
                command: Command::Window(Operation::VirtualNumber(1)),
            },
            Event::Command {
                command: Command::Window(Operation::VirtualNumber(0)),
            },
            // Focus window 0 (top of the 2-item stack), then press South twice.
            Event::Command {
                command: Command::Window(Operation::Focus(Direction::First)),
            },
            Event::Command {
                command: Command::Window(Operation::FocusOrVirtual(Direction::South)),
            },
            Event::Command {
                command: Command::Window(Operation::FocusOrVirtual(Direction::South)),
            },
        ];

        TestHarness::new()
            .with_windows(2)
            .on_iteration(6, |world, _state| {
                // First South: a sibling exists below window 0 in the stack, so
                // focus must move to it instead of switching virtual workspace.
                assert_focused!(world, 1);
                assert_eq!(
                    active_virtual_index(world),
                    0,
                    "focusing a stack sibling must not switch virtual workspace"
                );
            })
            .on_iteration(7, |world, _state| {
                // Second South: window 1 is the last stack item, nothing left to
                // focus, so this must fall through to switching workspaces.
                assert_eq!(
                    active_virtual_index(world),
                    1,
                    "with nothing left to focus, South must fall through to switching workspace"
                );
            })
            .run(commands);
    }
    ```
*   **Pure Functions:** Extract complex layout math into pure functions (e.g., in `src/ecs/layout.rs`) and add unit tests.

## 6. Code Cleanup & Verification

Before concluding a task, creating a commit, or presenting work as complete, agents MUST run the standard cleanup and verification suite:

1. **Format Code:**
   ```sh
   cargo fmt
   ```
   Ensures all code in the crate complies with standard formatting rules and contains proper trailing newlines. Verify with `cargo fmt --check`.

2. **Lint with Clippy:**
   ```sh
   cargo clippy --all-targets -- -D warnings
   ```
   Paneru enforces strict lints. Fix all warnings; do not leave warnings unaddressed.

3. **Run All Tests:**
   ```sh
   cargo test --all-targets
   ```
   Ensure the complete test suite passes without regressions across the crate. For targeted debugging of a specific test:
   ```sh
   RUST_LOG=debug cargo test <test_name> -- --nocapture
   ```

4. **Live Check (when the change affects the running daemon):**
   ```sh
   scripts/install.sh
   ```
   Builds with `cargo install --locked`, signs with the `paneru-dev` identity so macOS keeps the Accessibility and Screen Recording grants, restarts the daemon and prints the signature. The one-time certificate setup is in README › "Keeping permissions across rebuilds".

## 7. Contribution Workflow

*   **Testing branch:** Use the `testing` branch as a base for the PR, unless the change is very small or is an urgent fix for an issue in the `main` branch. This way the changes get additional baking before unleashin them into the `main` population.
*   **Research:** Before implementing, check `src/ecs/...` to see if a similar system already exists.
*   **Implementation:** Follow the **Plan -> Act -> Validate** cycle.
*   **Verification:** Execute the cleanup and verification steps in Section 6. If changes affect window tiling or animation, verify that existing layout interactions continue to work as expected.

