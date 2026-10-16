//! niri's focus hand-off when a window leaves its row (it closes, hides,
//! minimizes or moves away): the next window down its stack, else up;
//! otherwise the column it was opened beside, else the column that slides into
//! its index, else its left neighbour.

use std::time::Duration;

use bevy::prelude::*;

use crate::assert_focused;
use crate::commands::{Command, MoveFocus, Operation};
use crate::ecs::focus::FocusHistory;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::workspace::PreviousStripPosition;
use crate::ecs::{ActiveWorkspaceMarker, FocusedMarker, SpawnWindowTrigger};
use crate::events::Event;
use crate::manager::Window;
use crate::platform::{ProcessSerialNumber, WinID};

use super::interaction::{active_columns, manage, settle};
use super::*;

/// The ids of every window holding the focus marker.
fn focused_windows(world: &mut World) -> Vec<WinID> {
    let mut query = world.query_filtered::<&Window, With<FocusedMarker>>();
    query.iter(world).map(|window| window.id()).collect()
}

/// Stacks the focused window onto the column to its left.
fn stack() -> Event {
    Event::Command {
        command: Command::Window(Operation::Stack(true)),
    }
}

/// The app opens window `id`, which takes focus.
pub(super) fn open_window(world: &mut World, state: &MockState, id: WinID) {
    let frame = IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, id, frame);
    world.trigger(SpawnWindowTrigger(vec![window]));
    state.focus_window(id);
}

/// Closing the active column focuses the column that slides into its index.
#[test]
fn closing_a_column_focuses_the_one_that_slides_into_its_place() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 1);
            state.os_close_window(1);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 2]);
            assert_focused!(world, 2);
        })
        .run(vec![settle(), settle(), settle()]);
}

/// Closing the last column focuses its left neighbour.
#[test]
fn closing_the_last_column_focuses_its_left_neighbour() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(2))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 2);
            state.os_close_window(2);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 1);
        })
        .run(vec![settle(), settle(), settle()]);
}

/// niri's `activate_prev_column_on_removal`: a window opened right of the
/// active column and closed straight away hands focus back to that column.
#[test]
fn closing_a_just_opened_window_returns_to_the_column_it_opened_from() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            open_window(world, &state, 2);
        })
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2, 1]);
            assert_focused!(world, 2);
            state.os_close_window(2);
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 0);
        })
        .run(vec![settle(), settle(), settle(), settle()]);
}

/// Focusing another tiled window drops the way back to the opener column, so
/// the just-opened window then hands focus to the column sliding into place.
#[test]
fn focusing_away_drops_the_way_back_to_the_opener_column() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| open_window(world, &state, 2))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2, 1]);
            state.focus_window(1);
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 1);
            state.focus_window(2);
        })
        .on_iteration(4, |world, state| {
            assert_focused!(world, 2);
            state.os_close_window(2);
        })
        .on_iteration(5, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 1);
        })
        .run(vec![
            settle(),
            settle(),
            settle(),
            settle(),
            settle(),
            settle(),
        ]);
}

/// Closing a stacked window focuses the next one down its stack, not a
/// neighbour column.
#[test]
fn closing_the_top_of_a_stack_focuses_the_window_below() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2], "1 stacked under 0");
            state.focus_window(0);
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 0);
            state.os_close_window(0);
        })
        .on_iteration(4, |world, _state| {
            assert_eq!(active_columns(world), vec![1, 2]);
            assert_focused!(world, 1);
        })
        .run(vec![settle(), settle(), stack(), settle(), settle()]);
}

/// The bottom of a stack has nothing below it, so focus goes up the stack.
#[test]
fn closing_the_bottom_of_a_stack_focuses_the_window_above() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2], "1 stacked under 0");
            assert_focused!(world, 1);
            state.os_close_window(1);
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 2]);
            assert_focused!(world, 0);
        })
        .run(vec![settle(), settle(), stack(), settle()]);
}

/// A floating window hands focus back to the active tiled column.
#[test]
fn closing_a_floating_window_focuses_the_active_column() {
    TestHarness::new()
        .with_windows(4)
        .on_iteration(0, |_world, state| state.focus_window(3))
        .on_iteration(1, |world, _state| assert_focused!(world, 3))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 1, 2], "3 floats");
            state.focus_window(0);
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 0);
            state.focus_window(3);
        })
        .on_iteration(4, |world, state| {
            assert_focused!(world, 3);
            state.os_close_window(3);
        })
        .on_iteration(5, |world, _state| assert_focused!(world, 0))
        .run(vec![
            settle(),
            settle(),
            manage(),
            settle(),
            settle(),
            settle(),
        ]);
}

/// A background window closing on its own leaves the focus where it is.
#[test]
fn closing_a_background_window_keeps_the_focus() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(2))
        .on_iteration(1, |_world, state| state.focus_window(0))
        // Let the close grace on window 2 run out.
        .on_iteration(3, |world, state| {
            assert_focused!(world, 0);
            state.os_close_window(2);
        })
        .on_iteration(4, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 0);
        })
        .run(vec![settle(), settle(), settle(), settle(), settle()]);
}

/// An app quitting takes its windows down with it, skipping the destroy
/// events; its focused window still hands focus on by the same rule.
#[test]
fn an_app_quitting_hands_focus_to_the_neighbour_column() {
    const OTHER_PID: i32 = TEST_PROCESS_ID + 1;
    let mut harness =
        TestHarness::new()
            .with_windows(2)
            .with_app(OTHER_PID, "other", "Other", |_| {});
    let frame = IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let window = harness
        .mock_state
        .spawn_window(OTHER_PID, TEST_WORKSPACE_ID, 2, frame);
    harness.world().trigger(SpawnWindowTrigger(vec![window]));
    let quit = Event::ApplicationTerminated {
        psn: ProcessSerialNumber {
            high: 0,
            low: OTHER_PID.cast_unsigned(),
        },
    };

    harness
        .on_iteration(0, |_world, state| state.focus_window(2))
        .on_iteration(1, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1, 2]);
            assert_focused!(world, 2);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 1);
        })
        .on_iteration(4, |world, _state| assert_focused!(world, 1))
        .run(vec![settle(), settle(), quit, settle(), settle()]);
}

/// An app closing one of its windows can focus its own next window before the
/// close reaches us; the closing window still hands focus on by the rule.
#[test]
fn a_close_right_after_the_app_moved_focus_still_hands_it_on() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 1);
            state.focus_window(0);
            state.os_close_window(1);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 2]);
            assert_focused!(world, 2);
        })
        .run(vec![settle(), settle(), settle()]);
}

/// Two windows dying in one frame: the second is closing before the first
/// one's hand-off has landed, and still hands focus on by the rule.
#[test]
fn two_windows_closing_in_one_frame_leave_one_focused_successor() {
    TestHarness::new()
        .with_windows(4)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 1);
            state.os_close_window(1);
            state.os_close_window(2);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 3]);
            assert_eq!(focused_windows(world), vec![3]);
        })
        .run(vec![settle(), settle(), settle()]);
}

/// Closing the only window leaves nothing to focus: the row stays active and
/// empty, and the lost-focus safety net has nothing to recover to.
#[test]
fn closing_the_only_window_leaves_the_empty_row_active() {
    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            state.os_close_window(0);
        })
        // Long enough for the once-a-second safety net to run.
        .on_iteration(3, |world, _state| {
            assert!(focused_windows(world).is_empty());
            assert!(active_columns(world).is_empty());
            let mut query = world.query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>();
            assert_eq!(query.single(world).expect("an active row").virtual_index, 0);
        })
        .run(vec![settle(), settle(), settle(), settle()]);
}

/// The app focusing another window first doesn't lose the way back: a
/// just-opened window closing within the grace still returns to its opener.
#[test]
fn a_just_opened_window_closing_within_the_grace_returns_to_its_opener() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| open_window(world, &state, 2))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2, 1]);
            assert_focused!(world, 2);
            state.focus_window(1);
            state.os_close_window(2);
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1]);
            assert_focused!(world, 0);
        })
        .run(vec![settle(), settle(), settle(), settle()]);
}

/// A second app, `OTHER_PID`, with window 3 opened as the last column.
const OTHER_PID: i32 = TEST_PROCESS_ID + 1;

fn with_other_app_window(harness: TestHarness, id: WinID) -> TestHarness {
    let mut harness = harness.with_app(OTHER_PID, "other", "Other", |_| {});
    let frame = IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let window = harness
        .mock_state
        .spawn_window(OTHER_PID, TEST_WORKSPACE_ID, id, frame);
    harness.world().trigger(SpawnWindowTrigger(vec![window]));
    harness
}

/// Moves the focused window to virtual row 1.
fn move_to_row_1(move_focus: MoveFocus) -> Event {
    Event::Command {
        command: Command::Window(Operation::VirtualMoveNumber(1, move_focus)),
    }
}

/// Hiding an app whose window doesn't have focus leaves the focus where it is.
#[test]
fn hiding_a_window_without_focus_keeps_the_focus() {
    let hide = Event::ApplicationHidden { pid: OTHER_PID };

    with_other_app_window(TestHarness::new().with_windows(3), 3)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1, 2, 3]);
            assert_focused!(world, 0);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 1, 2]);
            assert_focused!(world, 0);
        })
        .run(vec![settle(), settle(), hide]);
}

/// Minimizing the focused middle column focuses the column that slides into
/// its place, like closing it does.
#[test]
fn minimizing_the_focused_middle_column_focuses_the_right_neighbour() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 1);
            state.os_minimize_window(1, true);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 2]);
            assert_focused!(world, 2);
        })
        .run(vec![settle(), settle(), settle()]);
}

/// Minimizing a stacked window focuses the next one down its stack.
#[test]
fn minimizing_the_top_of_a_stack_focuses_the_window_below() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 2], "1 stacked under 0");
            state.focus_window(0);
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 0);
            state.os_minimize_window(0, true);
        })
        .on_iteration(4, |world, _state| {
            assert_eq!(active_columns(world), vec![1, 2]);
            assert_focused!(world, 1);
        })
        .run(vec![settle(), settle(), stack(), settle(), settle()]);
}

/// Sending the focused middle column to another row focuses the column that
/// slides into its place.
#[test]
fn moving_the_focused_middle_column_to_another_row_focuses_the_right_neighbour() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| {
            assert_eq!(active_columns(world), vec![0, 2]);
            assert_focused!(world, 2);
        })
        .run(vec![settle(), settle(), move_to_row_1(MoveFocus::Stay)]);
}

/// Following the middle column to another row: the row it left remembers the
/// column that slid into its place, for when the user comes back.
#[test]
fn following_the_middle_column_to_another_row_remembers_the_right_neighbour() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 1);
            let right = find_window_entity(2, world);
            let mut rows = world.query::<(&LayoutStrip, Option<&PreviousStripPosition>)>();
            let remembered = rows
                .iter(world)
                .find(|(strip, _)| strip.virtual_index == 0)
                .expect("row 0")
                .1
                .expect("row 0 remembers where it was")
                .focus;
            assert_eq!(remembered, Some(right));
        })
        .run(vec![settle(), settle(), move_to_row_1(MoveFocus::Follow)]);
}

/// A window its app opens in the background doesn't take over as the active
/// column, so closing the focused window still hands focus on.
#[test]
fn closing_beside_a_window_opened_in_the_background_hands_focus_on() {
    let mut harness = TestHarness::new()
        .with_windows(2)
        .with_app(OTHER_PID, "other", "Other", |app| {
            app.is_frontmost = false;
        })
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            let frame = IRect::new(0, 0, TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
            let window = state.spawn_window(OTHER_PID, TEST_WORKSPACE_ID, 5, frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(2, |world, state| {
            assert_eq!(active_columns(world), vec![0, 5, 1]);
            assert_focused!(world, 0);
            let focused = find_window_entity(0, world);
            assert_eq!(
                world
                    .resource::<FocusHistory>()
                    .last_managed(TEST_WORKSPACE_ID),
                Some(focused),
                "the window in the background isn't the active column"
            );
            state.os_close_window(0);
        });
    harness.run(vec![settle(), settle(), settle()]);

    // Well short of the 1-second lost-focus recovery.
    harness.advance(Duration::from_millis(60));
    assert_focused!(harness.world(), 5);
}
