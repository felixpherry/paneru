//! Layout modes: `window layout toggle` cycles the row on screen through
//! scrolling → dwindle → master. Until frames follow the mode (Phase 2) these
//! tests assert the mode value only.

use bevy::prelude::*;

use crate::commands::{Command, Operation};
use crate::ecs::ActiveWorkspaceMarker;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::state::{SavedLayout, SavedStrip};
use crate::ecs::tiling::{DEFAULT_MASTER_RATIO, Tiling};
use crate::events::Event;

use super::*;

fn toggle() -> Event {
    Event::Command {
        command: Command::Window(Operation::ToggleLayout),
    }
}

/// The row on screen: whether it is a native fullscreen row, and its mode.
fn active_row(world: &mut World) -> (bool, Tiling) {
    let strip = world
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
        .single(world)
        .expect("exactly one active strip");
    (strip.is_fullscreen(), strip.tiling().clone())
}

/// The mode of the test Space's row `virtual_index`.
fn row_tiling(world: &mut World, virtual_index: u32) -> Tiling {
    world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == TEST_WORKSPACE_ID && strip.virtual_index == virtual_index)
        .expect("row exists")
        .tiling()
        .clone()
}

/// Spec test 1.
#[test]
fn toggle_cycles_scrolling_dwindle_master() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |world, _state| {
            assert_eq!(
                active_row(world).1,
                Tiling::Scrolling,
                "a new row starts in scrolling"
            );
        })
        .on_iteration(1, |world, _state| {
            assert!(matches!(active_row(world).1, Tiling::Dwindle(_)));
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_row(world).1, Tiling::Master(DEFAULT_MASTER_RATIO));
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(active_row(world).1, Tiling::Scrolling);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            toggle(),
            toggle(),
            toggle(),
        ]);
}

/// Spec test 2.
#[test]
fn toggle_changes_only_the_row_on_screen() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(3, |world, _state| {
            assert!(
                matches!(row_tiling(world, 0), Tiling::Dwindle(_)),
                "the row on screen cycles"
            );
            assert_eq!(
                row_tiling(world, 1),
                Tiling::Scrolling,
                "another row keeps its mode"
            );
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            // Visit row 1 and come back so it exists (no auto-create needed).
            Event::Command {
                command: Command::Window(Operation::VirtualNumber(1)),
            },
            Event::Command {
                command: Command::Window(Operation::VirtualNumber(0)),
            },
            toggle(),
        ]);
}

/// Spec test 3. Window 0 goes native fullscreen the way
/// `interaction.rs::native_fullscreen_transition_removes_window_from_original_strip_without_focus_marker`
/// does it; the fullscreen row then carries `ActiveWorkspaceMarker`.
#[test]
fn toggle_leaves_native_fullscreen_rows_alone() {
    const FULLSCREEN_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.update_window(0, |window| {
                window.workspace_id = FULLSCREEN_WORKSPACE_ID;
                window.is_full_screen = true;
            });
            state.activate_workspace(TEST_DISPLAY_ID, FULLSCREEN_WORKSPACE_ID, true);
        })
        .on_iteration(1, |world, _state| {
            assert!(active_row(world).0, "the fullscreen row is on screen");
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(
                active_row(world),
                (true, Tiling::Scrolling),
                "the toggle skips native fullscreen rows"
            );
        })
        .run(vec![
            Event::Command {
                command: Command::PrintState,
            },
            Event::SpaceChanged,
            toggle(),
        ]);
}

/// State files from before layout modes load as scrolling; the mode
/// serialises as a lowercase string and converts both ways.
#[test]
fn saved_layout_defaults_to_scrolling_and_round_trips() {
    let old: SavedStrip = serde_json::from_str(r#"{"virtual_index":0,"columns":[]}"#)
        .expect("a pre-layout-modes strip loads");
    assert_eq!(old.layout, SavedLayout::Scrolling);

    let master = SavedStrip {
        layout: SavedLayout::Master,
        ..old
    };
    let json = serde_json::to_string(&master).expect("serialises");
    assert!(json.contains(r#""layout":"master""#), "{json}");

    assert_eq!(
        SavedLayout::from(&Tiling::Dwindle(None)),
        SavedLayout::Dwindle
    );
    assert_eq!(
        Tiling::from(SavedLayout::Master),
        Tiling::Master(DEFAULT_MASTER_RATIO)
    );
}
