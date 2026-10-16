//! Layout modes: `window layout toggle` cycles the row on screen through
//! scrolling → dwindle → master. Tests 1–3 assert the mode value; the rest
//! assert frames, focus or overview tiles on the 1024×748 test viewport at
//! y = 20. Dwindle and master agree on two columns (side by side) and three
//! (one left, two stacked right), so the width and no-scroll tests hold in
//! either mode.

use bevy::prelude::*;

use crate::commands::{Command, Direction, Operation, ResizeDirection};
use crate::config::{Config, MainOptions, default_preset_column_widths};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::overview::Overview;
use crate::ecs::state::{SavedColumn, SavedLayout, SavedStrip};
use crate::ecs::tiling::{DEFAULT_MASTER_RATIO, Tiling};
use crate::ecs::{ActiveWorkspaceMarker, DockPosition, FullWidthMarker, ReshuffleAroundMarker};
use crate::events::Event;
use crate::manager::Size;
use crate::{assert_focused, assert_window_at, assert_window_size};

use super::close_focus::open_window;
use super::interaction::settle;
use super::session_restore::{saved_window, state_with_strips};
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

const TOP: i32 = TEST_MENUBAR_HEIGHT;

fn op(operation: Operation) -> Event {
    Event::Command {
        command: Command::Window(operation),
    }
}

/// Two windows as two half-screen tiles side by side.
fn two_halves(world: &mut World, _state: MockState) {
    assert_window_at!(world, 0, 0, TOP);
    assert_window_size!(world, 0, 512, 748);
    assert_window_at!(world, 1, 512, TOP);
    assert_window_size!(world, 1, 512, 748);
}

/// Three windows: the first column left, the other two stacked on the right.
fn three_tiles(world: &mut World, _state: MockState) {
    assert_window_at!(world, 0, 0, TOP);
    assert_window_size!(world, 0, 512, 748);
    assert_window_at!(world, 1, 512, TOP);
    assert_window_size!(world, 1, 512, 374);
    assert_window_at!(world, 2, 512, TOP + 374);
    assert_window_size!(world, 2, 512, 374);
}

/// Window 1 stacked under 0 in the left tile, window 2 alone on the right.
fn stacked_left_tile(world: &mut World, _state: MockState) {
    assert_window_at!(world, 0, 0, TOP);
    assert_window_size!(world, 0, 512, 374);
    assert_window_at!(world, 1, 0, TOP + 374);
    assert_window_size!(world, 1, 512, 374);
    assert_window_at!(world, 2, 512, TOP);
    assert_window_size!(world, 2, 512, 748);
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

/// Spec test 4.
#[test]
fn master_puts_the_first_column_left_and_stacks_the_rest() {
    TestHarness::new()
        .with_windows(4)
        .on_iteration(2, |world, _state| {
            assert_window_at!(world, 0, 0, TOP);
            assert_window_size!(world, 0, 512, 748);
            assert_window_at!(world, 1, 512, TOP);
            assert_window_size!(world, 1, 512, 249);
            assert_window_at!(world, 2, 512, TOP + 249);
            assert_window_size!(world, 2, 512, 249);
            assert_window_at!(world, 3, 512, TOP + 498);
            assert_window_size!(world, 3, 512, 250);
        })
        .run(vec![Event::MenuOpened { window_id: 0 }, toggle(), toggle()]);
}

/// Spec test 5. Default presets filtered to (0.05, 0.95): ¼ ⅓ ½ ⅔ ¾. The
/// harness's `Config::default()` has no presets (serde fills them in the real
/// config), so the test sets them.
#[test]
fn width_keys_step_the_master_width() {
    let config: Config = (
        MainOptions {
            preset_column_widths: default_preset_column_widths(),
            ..Default::default()
        },
        vec![],
    )
        .into();
    TestHarness::new()
        .with_config(config)
        .with_windows(2)
        .on_iteration(3, |world, _state| {
            assert_window_size!(world, 0, 683, 748);
            assert_window_at!(world, 1, 683, TOP);
            assert_window_size!(world, 1, 341, 748);
        })
        .on_iteration(4, two_halves)
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            toggle(),
            toggle(),
            op(Operation::Resize(ResizeDirection::Grow)),
            op(Operation::Resize(ResizeDirection::Shrink)),
        ]);
}

/// Spec test 6, in dwindle (steps 1–3) and master (steps 4–6).
#[test]
fn width_changes_leave_tiled_windows_alone() {
    let no_full_width = |world: &mut World, state: MockState| {
        two_halves(world, state);
        let entity = find_window_entity(0, world);
        assert!(
            world.get::<FullWidthMarker>(entity).is_none(),
            "full width never applies to a tiled window"
        );
    };
    TestHarness::new()
        .with_windows(2)
        .on_iteration(1, two_halves)
        .on_iteration(2, two_halves)
        .on_iteration(3, no_full_width)
        .on_iteration(4, two_halves)
        .on_iteration(5, two_halves)
        .on_iteration(6, no_full_width)
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            toggle(),
            op(Operation::SetWidth(0.3)),
            op(Operation::FullWidth),
            toggle(),
            op(Operation::SetWidth(0.3)),
            op(Operation::FullWidth),
        ]);
}

/// Spec test 7. Auto-center makes a scrolling reshuffle move a 1024-wide strip
/// off x = 0. A focus change (step 3) asks for both a reshuffle and an
/// ensure-visible, either of which pins the row, so step 3 also asks for a bare
/// reshuffle (what a new window or a stack move sends) and step 4 proves the
/// reshuffle pin on its own. Step 6 proves the swipe is dropped.
#[test]
fn tiled_rows_never_scroll() {
    let config: Config = (
        MainOptions {
            swipe_gesture_fingers: Some(3),
            auto_center: Some(true),
            ..Default::default()
        },
        vec![],
    )
        .into();
    TestHarness::new()
        .with_config(config)
        .with_windows(3)
        .on_iteration(1, |world, _state| {
            let mut query = world.query::<&crate::manager::Window>();
            let x = query
                .iter(world)
                .find(|window| window.id() == 0)
                .expect("window 0")
                .frame()
                .min
                .x;
            assert!(x < 0, "the strip starts scrolled past window 0");
        })
        .on_iteration(2, three_tiles)
        .on_iteration(3, |world, state| {
            three_tiles(world, state);
            let entity = find_window_entity(0, world);
            world.entity_mut(entity).insert(ReshuffleAroundMarker);
        })
        .on_iteration(4, three_tiles)
        .on_iteration(6, three_tiles)
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            op(Operation::Focus(Direction::Last)),
            toggle(),
            op(Operation::Focus(Direction::First)),
            Event::Command {
                command: Command::PrintState,
            },
            Event::Swipe {
                delta: 0.04,
                fingers: 3,
            },
            Event::Command {
                command: Command::PrintState,
            },
        ]);
}

/// Spec test 11, plus full width on the stacked tile, which must not unstack.
#[test]
fn stacked_column_shares_one_master_tile() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(4, stacked_left_tile)
        .on_iteration(5, |world, state| {
            stacked_left_tile(world, state);
            let (top, below) = (find_window_entity(0, world), find_window_entity(1, world));
            let strip = world
                .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
                .single(world)
                .expect("exactly one active strip");
            assert_eq!(
                strip.index_of(top).ok(),
                strip.index_of(below).ok(),
                "full width must not unstack a tile"
            );
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            op(Operation::Focus(Direction::East)),
            op(Operation::Stack(true)),
            toggle(),
            toggle(),
            op(Operation::FullWidth),
        ]);
}

/// A Dock change re-tiles a master row and moves it to the new viewport origin.
#[test]
fn dock_change_retiles_a_master_row() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(2, |world, _state| {
            let display = world
                .query_filtered::<Entity, With<crate::manager::Display>>()
                .single(world)
                .expect("one display");
            world.entity_mut(display).insert(DockPosition::Left(100));
        })
        .on_iteration(3, |world, _state| {
            assert_window_at!(world, 0, 100, TOP);
            assert_window_size!(world, 0, 462, 748);
            assert_window_at!(world, 1, 562, TOP);
            assert_window_size!(world, 1, 462, 748);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            toggle(),
            toggle(),
            Event::Command {
                command: Command::PrintState,
            },
        ]);
}

/// Spec tests 8 and 9. Window 3 opens beside the focused window 0 and splits
/// only 0's tile, taking its bottom half (the tile is taller than wide);
/// closing it gives 0 the whole tile back. Master's tiles, or a spiral rebuilt
/// from the column order, would keep 0 whole and move 1 and 2 instead.
#[test]
fn dwindle_open_splits_the_focused_tile_and_close_gives_it_back() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            three_tiles(world, state.clone());
            open_window(world, &state, 3);
        })
        .on_iteration(2, |world, state| {
            assert_window_at!(world, 0, 0, TOP);
            assert_window_size!(world, 0, 512, 374);
            assert_window_at!(world, 3, 0, TOP + 374);
            assert_window_size!(world, 3, 512, 374);
            assert_window_at!(world, 1, 512, TOP);
            assert_window_size!(world, 1, 512, 374);
            assert_window_at!(world, 2, 512, TOP + 374);
            assert_window_size!(world, 2, 512, 374);
            state.os_close_window(3);
        })
        .on_iteration(3, three_tiles)
        .run(vec![settle(), toggle(), settle(), settle()]);
}

/// Spec test 10, plus a swap across the row. A swap exchanges the two
/// columns, so on a dwindle row the two windows trade tiles and nothing
/// between them moves: at step 4 a move would also push window 0 down.
#[test]
fn dwindle_swap_trades_two_tiles() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 0);
            three_tiles(world, state);
        })
        .on_iteration(2, |world, _state| {
            assert_window_at!(world, 1, 0, TOP);
            assert_window_size!(world, 1, 512, 748);
            assert_window_at!(world, 0, 512, TOP);
            assert_window_size!(world, 0, 512, 374);
            assert_window_at!(world, 2, 512, TOP + 374);
            assert_window_size!(world, 2, 512, 374);
        })
        .on_iteration(3, |world, _state| assert_focused!(world, 2))
        .on_iteration(4, |world, _state| {
            assert_window_at!(world, 2, 0, TOP);
            assert_window_size!(world, 2, 512, 748);
            assert_window_at!(world, 0, 512, TOP);
            assert_window_size!(world, 0, 512, 374);
            assert_window_at!(world, 1, 512, TOP + 374);
            assert_window_size!(world, 1, 512, 374);
        })
        .run(vec![
            settle(),
            toggle(),
            op(Operation::Swap(Direction::East)),
            op(Operation::Focus(Direction::Last)),
            op(Operation::Swap(Direction::West)),
        ]);
}

/// Spec test 12, plus `FocusOrVirtual`. From the master, East picks the top
/// stack tile (both stack tiles are equally near; the earlier column wins).
/// South then moves down the stack and ↑ (`FocusOrVirtual`) back up, where
/// column order finds nothing.
#[test]
fn focus_follows_the_screen_in_master() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(2, |world, state| {
            assert_focused!(world, 0);
            three_tiles(world, state);
        })
        .on_iteration(3, |world, _state| assert_focused!(world, 1))
        .on_iteration(4, |world, _state| assert_focused!(world, 2))
        .on_iteration(5, |world, _state| assert_focused!(world, 1))
        .run(vec![
            settle(),
            toggle(),
            toggle(),
            op(Operation::Focus(Direction::East)),
            op(Operation::Focus(Direction::South)),
            op(Operation::FocusOrVirtual(Direction::North)),
        ]);
}

/// Spec test 13. On a master row, West from the bottom stack tile reaches the
/// master; the swap makes window 2 the master and puts 0 in 2's old tile,
/// while 1 stays put.
#[test]
fn swap_west_from_the_stack_makes_a_new_master() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(0, |_world, state| state.focus_window(0))
        .on_iteration(2, three_tiles)
        .on_iteration(3, |world, _state| assert_focused!(world, 2))
        .on_iteration(4, |world, _state| {
            assert_window_at!(world, 2, 0, TOP);
            assert_window_size!(world, 2, 512, 748);
            assert_window_at!(world, 1, 512, TOP);
            assert_window_size!(world, 1, 512, 374);
            assert_window_at!(world, 0, 512, TOP + 374);
            assert_window_size!(world, 0, 512, 374);
        })
        .run(vec![
            settle(),
            toggle(),
            toggle(),
            op(Operation::Focus(Direction::Last)),
            op(Operation::Swap(Direction::West)),
        ]);
}

/// Spec test 14. A tabbed stack's windows share one tile, so the screen finds
/// nothing below window 0; ↓ walks the stack first and lands on the next tab.
#[test]
fn south_cycles_a_tabbed_tile_before_the_screen() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(4, |world, _state| {
            assert_window_at!(world, 0, 0, TOP);
            assert_window_size!(world, 0, 512, 748);
            assert_window_at!(world, 1, 0, TOP);
            assert_window_size!(world, 1, 512, 748);
            assert_window_at!(world, 2, 512, TOP);
            assert_window_size!(world, 2, 512, 748);
        })
        .on_iteration(5, |world, _state| assert_focused!(world, 0))
        .on_iteration(6, |world, _state| assert_focused!(world, 1))
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            op(Operation::Focus(Direction::East)),
            op(Operation::Stack(true)),
            op(Operation::ToggleTabbedDisplay),
            toggle(),
            op(Operation::Focus(Direction::First)),
            op(Operation::Focus(Direction::South)),
        ]);
}

/// Row `[0] [1] [2+3]` in scrolling: window 1 at 600 wide (step 2), the
/// others at 400, and 2 and 3 splitting their column evenly.
fn scrolling_sizes(world: &mut World, _state: MockState) {
    assert_window_size!(world, 0, 400, 748);
    assert_window_size!(world, 1, 600, 748);
    assert_window_size!(world, 2, 400, 374);
    assert_window_size!(world, 3, 400, 374);
}

/// Spec test 15, with the stacked pair in its own column. In master the
/// stack sits in the bottom-right tile at 187/187; back in scrolling every
/// window gets its old size. Without the saved sizes the columns stay 512
/// wide and the stack comes back 187/561.
#[test]
fn back_to_scrolling_restores_the_old_sizes() {
    TestHarness::new()
        .with_windows(4)
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(4, scrolling_sizes)
        .on_iteration(6, |world, _state| {
            assert_window_size!(world, 0, 512, 748);
            assert_window_size!(world, 1, 512, 374);
            assert_window_size!(world, 2, 512, 187);
            assert_window_size!(world, 3, 512, 187);
        })
        .on_iteration(7, |world, state| {
            assert_eq!(active_row(world).1, Tiling::Scrolling);
            scrolling_sizes(world, state);
        })
        .run(vec![
            Event::MenuOpened { window_id: 0 },
            op(Operation::Focus(Direction::East)),
            op(Operation::SetWidth(0.585_937_5)),
            op(Operation::Focus(Direction::Last)),
            op(Operation::Stack(true)),
            toggle(),
            toggle(),
            toggle(),
        ]);
}

/// Spec test 16. A row saved in master comes back in master after a restart,
/// tiled as two half-screen tiles rather than its 400-wide scrolling columns.
#[test]
fn restart_restores_a_master_row() {
    let state = state_with_strips(vec![SavedStrip {
        layout: SavedLayout::Master,
        virtual_index: 0,
        columns: vec![
            SavedColumn::Single(saved_window(0)),
            SavedColumn::Single(saved_window(1)),
        ],
    }]);
    TestHarness::new()
        .with_windows(2)
        .with_state(state)
        .on_iteration(1, |world, state| {
            assert_eq!(active_row(world).1, Tiling::Master(DEFAULT_MASTER_RATIO));
            two_halves(world, state);
        })
        .run(idle(2));
}

/// Spec test 17. The overview draws a master row as its screen: at the
/// default ½ zoom the tiles fill the row's band on the 1024×748 viewport at
/// y = 20. Anchoring on the focused master column instead would shift every
/// tile 128 px right.
#[test]
fn overview_draws_a_master_row_as_its_screen() {
    TestHarness::new()
        .with_windows(3)
        .on_iteration(3, |world, _state| {
            let [w0, w1, w2] = [0, 1, 2].map(|id| find_window_entity(id, world));
            let overview = world.get_resource::<Overview>().expect("overview open");
            let (row, _) = overview.layout.find(w0).expect("tile");
            let target = |entity| overview.layout.find(entity).expect("tile").1.target;
            assert_eq!(
                overview.layout.rows[row].band,
                IRect::new(256, 207, 768, 581)
            );
            assert_eq!(target(w0), IRect::new(256, 207, 512, 581));
            assert_eq!(target(w1), IRect::new(512, 207, 768, 394));
            assert_eq!(target(w2), IRect::new(512, 394, 768, 581));
        })
        .run(vec![
            settle(),
            toggle(),
            toggle(),
            Event::Command {
                command: Command::Overview,
            },
        ]);
}

/// Spec Edge Cases (minimum size bigger than the tile). Window 1 can't be
/// narrower than 700, so the 512-wide right half can't hold it: it ends at
/// the screen's right edge, over window 0, and the layout stops asking it for
/// 512. Without the stop it is resized again on every idle step; without the
/// clamp it sits at x 512 and runs 188 px off screen.
#[test]
fn a_window_too_wide_for_its_tile_stays_on_screen_and_settles() {
    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            state.update_window(1, |window| window.min_size = Size::new(700, 0));
        })
        .on_iteration(2, |world, state| {
            assert_window_at!(world, 0, 0, TOP);
            assert_window_size!(world, 0, 512, 748);
            assert_window_at!(world, 1, TEST_DISPLAY_WIDTH - 700, TOP);
            assert_window_size!(world, 1, 700, 748);
            state.update_window(1, |window| window.resizes = 0);
        })
        .on_iteration(4, |_world, state| {
            state.update_window(1, |window| {
                assert_eq!(
                    window.resizes, 0,
                    "a refused tile size must not be asked again"
                );
            });
        })
        .run(vec![settle(), toggle(), settle(), settle(), settle()]);
}
