use std::time::Duration;

use bevy::prelude::*;

use crate::commands::{Command, MoveFocus, Operation};
use crate::config::{Config, MainOptions};
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{ActiveWorkspaceMarker, Bounds, SpawnWindowTrigger};
use crate::events::Event;
use crate::platform::WinID;
use crate::{assert_focused, assert_window_size};

use super::*;

pub(crate) fn spawn_matching_native_tab(world: &mut World, state: &MockState, window_id: WinID) {
    let leader = find_window_entity(window_id, world);
    let frame = world
        .get::<Window>(leader)
        .map(|window| window.frame())
        .expect("frame should exist");
    let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, window_id + 1, frame);

    // The previous window should now not be visible on screen.
    state.window_visible(window_id, false);

    world.trigger(SpawnWindowTrigger(vec![window]));
}

#[test]
fn test_native_tab_detection() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 }, // 0
        Event::Command {
            command: Command::PrintState,
        }, // 1
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(1, move |world, _state| {
            let follower = find_window_entity(0, world);
            let leader = find_window_entity(1, world);
            let strip = world
                .query::<&LayoutStrip>()
                .single(world)
                .expect("getting layout strip");
            assert!(strip.tabbed(follower));
            assert!(strip.tabbed(leader));
        })
        .run(commands);
}

#[test]
fn test_native_tab_resize_syncs_sibling_size() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(1, move |world, _state| {
            let tab_one = find_window_entity(1, world);
            let mut query = world.query::<&mut Bounds>();
            let mut bounds = query.get_mut(world, tab_one).expect("tab bounds missing");
            bounds.0.x = TEST_WINDOW_WIDTH + 160;
        })
        .on_iteration(2, move |world, _state| {
            assert_window_size!(
                world,
                0,
                TEST_WINDOW_WIDTH + 160,
                TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT
            );
            assert_window_size!(
                world,
                1,
                TEST_WINDOW_WIDTH + 160,
                TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT
            );
        })
        .run(commands);
}

#[test]
fn test_native_tab_virtual_move_moves_all_tabs() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Follow)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(3, move |world, _state| {
            let tab_zero = find_window_entity(0, world);
            let tab_one = find_window_entity(1, world);
            let mut query = world.query::<(&LayoutStrip, Has<ActiveWorkspaceMarker>)>();
            let active = query
                .iter(world)
                .find_map(|(strip, active)| active.then_some(strip))
                .expect("active strip not found");

            assert_eq!(active.virtual_index, 1);
            assert_eq!(active.len(), 1);
            assert_eq!(active.index_of(tab_zero).unwrap(), 0);
            assert_eq!(active.index_of(tab_one).unwrap(), 0);
            assert_eq!(active.tab_group(tab_zero), Some(vec![tab_one, tab_zero]));

            let mut query = world.query::<&LayoutStrip>();
            let source = query
                .iter(world)
                .find(|strip| strip.virtual_index == 0)
                .expect("source strip not found");
            assert!(!source.contains(tab_zero));
            assert!(!source.contains(tab_one));
        })
        .run(commands);
}

#[test]
fn test_native_tab_removal_keeps_remaining_window_column() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(1, move |world, _state| {
            let tab_one = find_window_entity(1, world);
            world.entity_mut(tab_one).despawn();
        })
        .on_iteration(2, move |world, _state| {
            let tab_zero = find_window_entity(0, world);
            let mut query = world.query::<(&LayoutStrip, Has<ActiveWorkspaceMarker>)>();
            let strip = query
                .iter(world)
                .find_map(|(strip, active)| active.then_some(strip))
                .expect("active strip not found");

            assert_eq!(strip.len(), 1, "removing a tab should not empty the column");
            assert_eq!(strip.tab_group(tab_zero), None);
            assert_eq!(strip.index_of(tab_zero).unwrap(), 0);
        })
        .run(commands);
}

#[test]
fn test_offscreen_same_app_same_width_different_frame_not_tabbed() {
    // Regression: a same-app sibling that happens to be off-screen (scrolled out of
    // the strip, on another space, etc.) with merely the same width as the new
    // window must not be misidentified as a native tab. Real native tabs share the
    // leader's full frame; partial matches must not trigger conversion.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            let leader = find_window_entity(0, world);
            let leader_frame = world
                .get::<Window>(leader)
                .map(|window| window.frame())
                .expect("frame should exist");

            // Same width as the leader, but a different origin and a different
            // height — what a freshly-spawned same-app window typically looks like.
            let mut frame = leader_frame;
            frame.min.y += 50;
            frame.max.y = frame.min.y + leader_frame.height() / 2;

            let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 1, frame);
            // Pretend the existing window is off-screen so the only thing preventing
            // a false positive is the frame check.
            state.window_visible(0, false);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(1, move |world, _state| {
            let leader = find_window_entity(0, world);
            let new_window = find_window_entity(1, world);
            let strip = world
                .query::<&LayoutStrip>()
                .single(world)
                .expect("getting layout strip");
            assert!(!strip.tabbed(leader));
            assert!(!strip.tabbed(new_window));
            assert_eq!(
                strip.len(),
                2,
                "non-matching frames must get their own column"
            );
        })
        .run(commands);
}

#[test]
fn test_disable_native_tabs_skips_detection() {
    // Setting `disable_native_tabs = true` must short-circuit the detector even
    // when the new window's frame matches the leader exactly.
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    let options = MainOptions {
        disable_native_tabs: Some(true),
        ..MainOptions::default()
    };
    let config: Config = (options, vec![]).into();

    TestHarness::new()
        .with_config(config)
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(1, move |world, _state| {
            let leader = find_window_entity(0, world);
            let new_window = find_window_entity(1, world);
            let strip = world
                .query::<&LayoutStrip>()
                .single(world)
                .expect("getting layout strip");
            assert!(!strip.tabbed(leader));
            assert!(!strip.tabbed(new_window));
            assert_eq!(
                strip.len(),
                2,
                "detector must be disabled — windows stay in separate columns"
            );
        })
        .run(commands);
}

#[test]
fn test_same_app_same_frame_native_tab_reuses_existing_column() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .on_iteration(0, move |world, state| {
            spawn_matching_native_tab(world, &state, 0);
        })
        .on_iteration(1, move |world, _state| {
            let tab_zero = find_window_entity(0, world);
            let tab_one = find_window_entity(1, world);
            let mut query = world.query::<(&LayoutStrip, Has<ActiveWorkspaceMarker>)>();
            let strip = query
                .iter(world)
                .find_map(|(strip, active)| active.then_some(strip))
                .expect("active strip not found");

            assert_eq!(strip.len(), 1, "native tab should not add a column");
            assert_eq!(strip.tab_group(tab_zero), Some(vec![tab_one, tab_zero]));
            assert_eq!(strip.tab_group(tab_one), Some(vec![tab_one, tab_zero]));
            assert_window_size!(
                world,
                0,
                TEST_WINDOW_WIDTH,
                TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT
            );
            assert_window_size!(
                world,
                1,
                TEST_WINDOW_WIDTH,
                TEST_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT
            );
        })
        .run(commands);
}

/// A native tab group left with one member is no tab group: closing that last
/// window hands focus on like closing any other window.
#[test]
fn closing_a_lone_native_tab_hands_focus_on() {
    let idle = || Event::Command {
        command: Command::PrintState,
    };
    let commands = vec![
        idle(),
        idle(),
        Event::Command {
            command: Command::Window(Operation::Stack(true)),
        },
        idle(),
        idle(),
    ];

    let mut harness = TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |world, state| {
            // Window 2 opens as a native tab of window 1 and takes focus.
            spawn_matching_native_tab(world, &state, 1);
            state.focus_window(2);
        })
        .on_iteration(2, |world, state| {
            let windows = [0, 1, 2].map(|id| find_window_entity(id, world));
            let strip = world
                .query::<&LayoutStrip>()
                .single(world)
                .expect("getting layout strip");
            assert_eq!(strip.len(), 1, "one column holds every window");
            assert!(windows.iter().all(|window| strip.contains(*window)));
            // The tab closes, leaving a tab group of one: Stack([0, Tabs([1])]).
            state.os_vanish_window(2);
            world.despawn(windows[2]);
            state.window_visible(1, true);
            state.focus_window(1);
        })
        .on_iteration(3, |world, _state| assert_focused!(world, 1));
    harness.run(commands);

    // Closing window 1 must hand focus on itself: check well short of the
    // 1-second lost-focus recovery, which would mask a missing hand-off.
    let window = find_window_entity(1, harness.world());
    harness.mock_state.os_vanish_window(1);
    harness.world().despawn(window);
    harness.advance(Duration::from_millis(60));
    assert_focused!(harness.world(), 0);
}
