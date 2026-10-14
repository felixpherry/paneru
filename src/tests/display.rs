use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use crate::commands::{Command, Direction, MouseMove, MoveFocus, Operation};
use crate::config::{Config, MainOptions};
use crate::ecs::display::FloatingLayer;
use crate::ecs::focus::FocusHistory;
use crate::ecs::layout::{LayoutStrip, PARKED_STRIP_SLIVER};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, DockPosition, NativeFullscreenMarker,
    SpawnWindowTrigger, Timeout,
};
use crate::events::Event;
use crate::manager::{Display, Origin, Size, Window};
use crate::platform::WinID;
use crate::{
    assert_focused, assert_not_on_workspace, assert_on_workspace, assert_window_at,
    assert_window_size,
};

use super::*;

#[test]
fn test_multi_display_lifecycle() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::DisplayRemoved {
            display_id: TEST_DISPLAY_ID,
        },
        Event::DisplayAdded {
            display_id: TEST_DISPLAY_ID,
        },
    ];

    let mut harness = TestHarness::new().with_windows(1);
    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    harness
        .on_iteration(1, |world, state| {
            let mut query = world.query_filtered::<Entity, With<Display>>();
            query.single(world).expect("should have one display");
            state.remove_display(TEST_DISPLAY_ID);
        })
        .on_iteration(2, |world, mut state| {
            assert!(
                world
                    .query_filtered::<Entity, With<Display>>()
                    .single(world)
                    .is_err(),
                "display should be despawned"
            );

            let workspace_entity = {
                let mut query = world.query_filtered::<Entity, With<LayoutStrip>>();
                query.single(world).expect("should have one workspace")
            };
            let workspace = world.entity(workspace_entity);
            assert!(
                workspace.get::<Timeout>().is_some(),
                "orphaned workspace should have a timeout"
            );
            assert!(
                workspace.get::<ChildOf>().is_none(),
                "orphaned workspace should have no parent"
            );
            state.add_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![TEST_WORKSPACE_ID],
            );
        })
        .on_iteration(3, |world, _state| {
            let new_display_entity = world
                .query_filtered::<Entity, With<Display>>()
                .single(world)
                .expect("display should be spawned again");

            let workspace_entity = {
                let mut query = world.query_filtered::<Entity, With<LayoutStrip>>();
                query.single(world).expect("should have one workspace")
            };
            let workspace = world.entity(workspace_entity);
            assert!(
                workspace.get::<Timeout>().is_none(),
                "re-parented workspace should no longer have a timeout"
            );
            let child_of: &ChildOf = workspace
                .get::<ChildOf>()
                .expect("re-parented workspace should have a parent");
            assert_eq!(
                child_of.parent(),
                new_display_entity,
                "workspace should be child of the new display"
            );
        })
        .run(commands);
}

#[test]
fn test_multi_workspace_orphaning() {
    let mut commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::DisplayRemoved {
            display_id: TEST_DISPLAY_ID,
        },
    ];
    commands.extend((0..6).map(|_| Event::Command {
        command: Command::PrintState,
    }));

    let workspaces = vec![TEST_WORKSPACE_ID, TEST_WORKSPACE_ID + 1];
    let harness = TestHarness::new().with_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        workspaces,
    );
    harness
        .on_iteration(1, |world, state| {
            let display_entity = world
                .query_filtered::<Entity, With<Display>>()
                .single(world)
                .expect("should have one display");

            let workspace_entities = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .iter(world)
                .collect::<Vec<_>>();
            assert_eq!(workspace_entities.len(), 2, "should have two workspaces");

            for &ws in &workspace_entities {
                let child_of: &ChildOf = world
                    .entity(ws)
                    .get::<ChildOf>()
                    .expect("workspace should have parent");
                assert_eq!(child_of.parent(), display_entity);
            }
            state.remove_display(TEST_DISPLAY_ID);
        })
        .on_iteration(8, |world, _state| {
            let workspace_entities = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .iter(world)
                .collect::<Vec<_>>();
            for &ws in &workspace_entities {
                let entity: EntityRef = world.entity(ws);
                assert!(
                    entity.get::<Timeout>().is_some(),
                    "each workspace should have a timeout"
                );
                assert!(
                    entity.get::<ChildOf>().is_none(),
                    "each workspace should have no parent"
                );
            }
        })
        .run(commands);
}

#[test]
fn test_multi_display_no_height_crosstalk() {
    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, ext_frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 200, frame);

    let ext_usable_height = EXT_DISPLAY_HEIGHT - TEST_MENUBAR_HEIGHT;

    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::DisplayChanged,
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness
        .on_iteration(1, move |world, _state| {
            assert_window_size!(world, 100, TEST_WINDOW_WIDTH, ext_usable_height);
        })
        .on_iteration(2, |world, _state| {
            use crate::ecs::ActiveWorkspaceMarker;
            let mut strip_query =
                world.query_filtered::<&mut LayoutStrip, Without<ActiveWorkspaceMarker>>();
            for mut strip in strip_query.iter_mut(world) {
                strip.set_changed();
            }
        })
        .on_iteration(4, move |world, _state| {
            assert_window_size!(world, 100, TEST_WINDOW_WIDTH, ext_usable_height);
        })
        .run(commands);
}

#[test]
fn test_next_display_inserts_into_target_strip() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::ToNextDisplay(MoveFocus::Follow)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_windows(1)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .on_iteration(1, move |world, _state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
        })
        .on_iteration(2, move |world, _state| {
            assert_on_workspace!(world, 0, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 0, TEST_WORKSPACE_ID);
        })
        .run(commands);
}

#[test]
fn test_send_next_display_stays_on_source() {
    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 101, frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 100, frame);

    let commands = vec![
        Event::MenuOpened { window_id: 101 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::ToNextDisplay(MoveFocus::Stay)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness
        .on_iteration(1, move |world, _state| {
            assert_on_workspace!(world, 100, TEST_WORKSPACE_ID);
        })
        .on_iteration(2, move |world, state| {
            assert_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_eq!(state.active_display(), TEST_DISPLAY_ID);
        })
        .run(commands);
}

#[test]
fn test_mouse_to_next_display() {
    let commands = vec![
        Event::MenuOpened { window_id: 101 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Mouse(MouseMove::ToNextDisplay),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];
    let origin = Origin::new(0, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let display_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);

    // harness
    //     .mock_state
    //     .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 101, frame);
    // harness
    //     .mock_state
    //     .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 100, frame);
    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, display_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| {
            data.pid = TEST_PROCESS_ID;
            data.workspace_id = TEST_WORKSPACE_ID;
            data.frame = frame;
        })
        .on_iteration(1, move |world, state| {
            let entity = find_window_entity(100, world);
            let window = world.get::<Window>(entity).expect("need window");
            assert_eq!(state.cursor_position(), window.frame().center());
        })
        .on_iteration(3, move |world, state| {
            let mut query = world.query::<(&Display, Option<&DockPosition>)>();
            let (display, dock) = query
                .iter(world)
                .find(|display| display.0.id() == EXT_DISPLAY_ID)
                .expect("need display");
            let config = world.resource::<Config>();
            let bounds = display.actual_display_bounds(dock, config);
            assert_eq!(state.cursor_position(), bounds.center());
        })
        .run(commands);
}

/// `mouse display north` goes by where focus is, not by the pointer: with
/// the pointer already resting on the display above, it still moves focus up
/// there (`mouse nextdisplay` would pick the display without the pointer, the
/// one focus is already on).
#[test]
fn test_mouse_to_display_north_ignores_the_pointer() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .with_workspace_window(200, EXT_WORKSPACE_ID, |data| data.frame = ext_frame)
        .on_iteration(1, move |world, state| {
            assert_focused!(world, 100);
            state.set_cursor_position(ext_bounds.center());
        })
        .on_iteration(3, move |world, state| {
            assert_focused!(world, 200);
            assert!(ext_bounds.contains(state.cursor_position()));
        })
        .run(commands);
}

/// An empty display has no window to focus, so the pointer is parked in its
/// middle and its desktop is focused, which makes it the active display.
#[test]
fn test_mouse_to_empty_display_and_back() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        },
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::South)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(1, move |world, state| {
            assert!(ext_bounds.contains(state.cursor_position()));
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            let mut active = world.query_filtered::<&Display, With<ActiveDisplayMarker>>();
            let active = active.single(world).expect("one active display");
            assert_eq!(active.id(), EXT_DISPLAY_ID);
        })
        .on_iteration(3, move |world, state| {
            assert!(!ext_bounds.contains(state.cursor_position()));
            assert_focused!(world, 100);
        })
        .run(commands);
}

/// Firefox's `--new-window` remote brings Firefox to the front with the new
/// window already focused, but places that window next to its others on the
/// laptop. macOS follows it there before paneru sees the window, which then
/// tiled into the laptop strip although the user was on the external display.
#[test]
fn test_window_opened_from_background_tiles_on_the_display_the_user_was_on() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command {
            command: Command::PrintState,
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::DisplayChanged,                // 4
        Event::Command {
            command: Command::PrintState,
        }, // 5
        Event::Command {
            command: Command::PrintState,
        }, // 6
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(Origin::new(0, 0), Origin::new(0, 0) + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, |_world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            // The app comes to the front with a window paneru can't see yet...
            state.front_switch(TEST_PROCESS_ID, 300);
        })
        .on_iteration(3, move |_world, state| {
            // ...which sits on the laptop, so macOS activates the laptop.
            state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 300, frame);
            state.set_active_display(TEST_DISPLAY_ID);
        })
        .on_iteration(4, |world, state| {
            let window = state.create_window(300);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(6, |world, _state| {
            assert_on_workspace!(world, 300, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 300, TEST_WORKSPACE_ID);
        })
        .run(commands);
}

/// The same as above, but macOS reports the display change first and in the
/// same batch as the front switch, as it did on a real Dell-over-laptop setup.
/// By the time the front switch is handled the laptop strip is already active.
#[test]
fn test_window_opened_from_background_survives_display_change_reported_first() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command {
            command: Command::PrintState,
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::Command {
            command: Command::PrintState,
        }, // 4
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(Origin::new(0, 0), Origin::new(0, 0) + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, move |_world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 300, frame);
            state.set_active_display(TEST_DISPLAY_ID);
            state.queue_event(Event::DisplayChanged);
            state.queue_event(Event::SpaceChanged);
            state.front_switch(TEST_PROCESS_ID, 300);
        })
        .on_iteration(4, |world, _state| {
            assert_on_workspace!(world, 300, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 300, TEST_WORKSPACE_ID);
        })
        .run(commands);
}

/// Firefox's `--new-window`, started from a binding while the user is on the
/// external display: Firefox comes to the front naming one of its old laptop
/// windows, macOS activates the laptop, and the new window spawns there.
/// `press` is the command the user's key sent before all that.
fn run_window_opened_after_display_moved(press: Command, workspace: WorkspaceId) {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command { command: press },    // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::Command {
            command: Command::PrintState,
        }, // 4
        Event::Command {
            command: Command::PrintState,
        }, // 5
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(Origin::new(0, 0), Origin::new(0, 0) + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, |_world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            state.set_active_display(TEST_DISPLAY_ID);
            state.queue_event(Event::DisplayChanged);
            state.queue_event(Event::SpaceChanged);
            state.front_switch(TEST_PROCESS_ID, 100);
        })
        .on_iteration(4, move |world, state| {
            let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 300, frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(5, move |world, _state| {
            assert_on_workspace!(world, 300, workspace);
        })
        .run(commands);
}

#[test]
fn test_window_launched_from_binding_tiles_where_the_binding_was_pressed() {
    run_window_opened_after_display_moved(Command::Lua(1), EXT_WORKSPACE_ID);
}

/// Firefox's `--new-window` from a binding on the external display, with the
/// new window and macOS's switch to the laptop arriving together. `spawn`:
/// the window is tiled on the external display before the switch is handled,
/// and the laptop's moved-window check, which still sees it on the laptop's
/// space until its move lands, must not pull it back. Otherwise paneru finds
/// it through the front switch. (A window adopted before it was tiled at all,
/// as seen live, needs an AX create event mid-frame, which the mock can't do.)
fn run_window_launched_with_display_change(spawn: bool) {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command {
            command: Command::Lua(1),
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::Command {
            command: Command::PrintState,
        }, // 4
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(Origin::new(0, 0), Origin::new(0, 0) + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, move |world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            let window = state.spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 300, frame);
            state.set_active_display(TEST_DISPLAY_ID);
            state.queue_event(Event::DisplayChanged);
            state.queue_event(Event::SpaceChanged);
            if spawn {
                state.front_switch(TEST_PROCESS_ID, 100);
                world.trigger(SpawnWindowTrigger(vec![window]));
            } else {
                state.front_switch(TEST_PROCESS_ID, 300);
            }
        })
        .on_iteration(4, |world, _state| {
            assert_on_workspace!(world, 300, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 300, TEST_WORKSPACE_ID);
        })
        .run(commands);
}

#[test]
fn test_window_launched_and_tiled_before_the_display_change() {
    run_window_launched_with_display_change(true);
}

#[test]
fn test_window_launched_and_found_during_the_display_change() {
    run_window_launched_with_display_change(false);
}

/// Without a binding press this is a Cmd-Tab to the app on the laptop, so a
/// window it opens belongs on the laptop.
#[test]
fn test_window_opened_after_app_activation_tiles_where_it_opened() {
    run_window_opened_after_display_moved(Command::PrintState, TEST_WORKSPACE_ID);
}

/// Closing the last window on the external display: macOS hands focus to
/// another window of the same app (101) on the laptop, but the user was last
/// on 100 there, so focus returns to 100.
#[test]
fn test_closing_last_window_on_display_focuses_the_other_displays_last_window() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command {
            command: Command::PrintState,
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::Command {
            command: Command::PrintState,
        }, // 4
        Event::Command {
            command: Command::PrintState,
        }, // 5
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .with_window(101, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, move |world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            state.set_focused_window(300);
            let window = state.spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 300, ext_frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
        })
        .on_iteration(3, |world, state| {
            assert_focused!(world, 300);
            state.os_close_window(300);
            state.set_focused_window(101);
            state.set_active_display(TEST_DISPLAY_ID);
            state.queue_event(Event::DisplayChanged);
            state.queue_event(Event::WindowFocused { window_id: 101 });
        })
        .on_iteration(5, |world, _state| {
            assert_focused!(world, 100);
        })
        .run(commands);
}

/// kitty opens a window through its single-instance server and then
/// activates, which re-keys the window it had before. Here that old window is
/// on the laptop and the new one on the external display, so following kitty's
/// report carried focus, and the active display, back to the laptop.
#[test]
fn test_new_window_keeps_focus_when_app_reports_its_window_on_another_display() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 }, // 0
        Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
        }, // 1
        Event::Command {
            command: Command::PrintState,
        }, // 2
        Event::Command {
            command: Command::PrintState,
        }, // 3
        Event::Command {
            command: Command::PrintState,
        }, // 4
    ];
    let ext_bounds = IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds, vec![EXT_WORKSPACE_ID])
        .with_window(100, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, move |world, state| {
            assert_eq!(state.active_display(), EXT_DISPLAY_ID);
            // kitty is already frontmost and still names its laptop window.
            state.set_focused_window(100);
            let window = state.spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 300, ext_frame);
            world.trigger(SpawnWindowTrigger(vec![window]));
            state.set_active_display(TEST_DISPLAY_ID);
            state.queue_event(Event::DisplayChanged);
        })
        .on_iteration(4, |world, _state| {
            assert_on_workspace!(world, 300, EXT_WORKSPACE_ID);
            assert_focused!(world, 300);
        })
        .run(commands);
}

/// Regression test: paneru's init pass must not drag windows that live on
/// inactive displays onto the active display. `apply_window_properties`
/// initially appends every observed window to the active strip; if the
/// layout writers run before `finish_setup` has reassigned them, they
/// cache active-display coordinates into `Position` and `commit_window_position`
/// later pushes those to macOS, moving the windows.
#[test]
fn test_init_keeps_windows_on_their_real_displays() {
    // Internal (test) display is active. Window 100 lives on the external
    // display's space, window 200 lives on the active display's space.

    let mut harness = TestHarness::new();
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    let origin = Origin::new(0, 0);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let frame = IRect::from_corners(origin, origin + size);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);

    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, TEST_WORKSPACE_ID, 200, ext_frame);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, frame);

    let commands = vec![
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness
        .on_iteration(0, move |world, _state| {
            assert_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_on_workspace!(world, 200, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 200, EXT_WORKSPACE_ID);
            // The OS frame for window 100 must stay within the external
            // display's vertical bounds (negative y); if init moved it
            // onto the active display the frame would land at y >= 0.
            assert_window_at!(world, 100, ext_origin.x, ext_origin.y);
        })
        .run(commands);
}

/// Waking from sleep (or a resolution/configuration change) with a monitor
/// gone should reconcile the ECS display set against the OS even though no
/// per-display `DisplayRemoved` flag arrives: the vanished display is removed
/// and its workspace is orphaned.
#[test]
fn test_wake_reconciles_unplugged_display() {
    let harness = TestHarness::new().with_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );

    // A window on the external display so its workspace strip actually exists.
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, ext_frame);

    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::PrintState,
        },
        Event::SystemWoke { msg: String::new() },
    ];

    harness
        .on_iteration(1, |world, state| {
            let displays = world
                .query_filtered::<Entity, With<Display>>()
                .iter(world)
                .count();
            assert_eq!(displays, 2, "should start with two displays");

            // Unplug the external display behind paneru's back — no
            // DisplayRemoved event is sent, mimicking a wake-from-sleep.
            state.remove_display(EXT_DISPLAY_ID);
        })
        .on_iteration(2, |world, _state| {
            let displays = world
                .query_filtered::<Entity, With<Display>>()
                .iter(world)
                .count();
            assert_eq!(displays, 1, "reconcile should despawn the vanished display");

            // The external display's workspace must be orphaned, not lost.
            let orphan = world
                .query::<(&LayoutStrip, Option<&ChildOf>, Has<Timeout>)>()
                .iter(world)
                .find(|(strip, _, _)| strip.id() == EXT_WORKSPACE_ID)
                .map(|(_, child, timeout)| (child.is_some(), timeout));
            let (has_parent, has_timeout) =
                orphan.expect("external workspace strip should still exist");
            assert!(!has_parent, "orphaned workspace should have no parent");
            assert!(has_timeout, "orphaned workspace should carry a timeout");
        })
        .run(commands);
}

#[test]
fn test_display_configuration_reapplies_layout_after_geometry_change() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.advance(Duration::from_millis(500));
    let window_entity = find_window_entity(0, harness.world());
    let original = harness
        .world()
        .get::<Window>(window_entity)
        .unwrap()
        .frame();

    let new_height = TEST_DISPLAY_HEIGHT - 100;
    harness.mock_state.set_display_bounds(
        TEST_DISPLAY_ID,
        IRect::new(120, 0, TEST_DISPLAY_WIDTH + 120, new_height),
    );
    harness
        .world()
        .write_message::<Event>(Event::DisplayConfigured {
            display_id: TEST_DISPLAY_ID,
        });

    harness.advance(Duration::from_millis(200));
    let world = harness.world();
    let display = world.query::<&Display>().single(world).unwrap();
    assert_eq!(display.bounds().min.x, 0, "wait for the display to settle");

    harness.advance(Duration::from_secs(1));
    let updated = harness
        .world()
        .get::<Window>(window_entity)
        .unwrap()
        .frame();
    assert_eq!(updated.min.x, original.min.x + 120);
    assert_eq!(updated.height(), new_height - TEST_MENUBAR_HEIGHT);
}

#[test]
fn test_display_configuration_catches_late_geometry_change() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.advance(Duration::from_millis(500));
    let window_entity = find_window_entity(0, harness.world());
    let original_x = harness
        .world()
        .get::<Window>(window_entity)
        .unwrap()
        .frame()
        .min
        .x;
    harness
        .world()
        .write_message::<Event>(Event::DisplayConfigured {
            display_id: TEST_DISPLAY_ID,
        });
    // The first settled scan still sees the old display configuration.
    harness.advance(Duration::from_millis(600));

    harness.mock_state.set_display_bounds(
        TEST_DISPLAY_ID,
        IRect::new(90, 0, TEST_DISPLAY_WIDTH + 90, TEST_DISPLAY_HEIGHT),
    );
    harness.advance(Duration::from_millis(1300));

    let world = harness.world();
    let display = world.query::<&Display>().single(world).unwrap();
    assert_eq!(display.bounds().min.x, 90);
    assert_eq!(
        world.get::<Window>(window_entity).unwrap().frame().min.x,
        original_x + 90
    );
}

#[test]
fn test_transient_empty_display_list_keeps_layout_and_retries() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.advance(Duration::from_millis(500));
    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    harness
        .world()
        .write_message::<Event>(Event::DisplayConfigured {
            display_id: TEST_DISPLAY_ID,
        });
    harness.advance(Duration::from_millis(600));

    let world = harness.world();
    let display_entity = world
        .query_filtered::<Entity, With<Display>>()
        .single(world)
        .unwrap();
    let strip_entity = world
        .query_filtered::<Entity, With<LayoutStrip>>()
        .single(world)
        .unwrap();
    assert_eq!(
        world.get::<ChildOf>(strip_entity).unwrap().parent(),
        display_entity
    );

    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(100, 0, TEST_DISPLAY_WIDTH + 100, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID],
    );
    harness.advance(Duration::from_millis(1600));
    let world = harness.world();
    let display = world.query::<&Display>().single(world).unwrap();
    assert_eq!(
        display.bounds().min.x,
        100,
        "retry should use the recovered display"
    );
}

#[test]
fn test_unplug_rehomes_workspace_and_window_on_surviving_display() {
    let mut harness = TestHarness::new().with_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let frame = IRect::from_corners(
        ext_origin,
        ext_origin + Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
    );
    harness
        .mock_state
        .spawn_window(TEST_PROCESS_ID, EXT_WORKSPACE_ID, 100, frame);
    harness.advance(Duration::from_millis(500));

    harness.mock_state.remove_display(EXT_DISPLAY_ID);
    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID, EXT_WORKSPACE_ID],
    );
    harness
        .world()
        .write_message::<Event>(Event::DisplayRemoved {
            display_id: EXT_DISPLAY_ID,
        });
    harness.advance(Duration::from_secs(1));

    let world = harness.world();
    let main = world.query::<(&Display, Entity)>().single(world).unwrap().1;
    let external_parent = world
        .query::<(&LayoutStrip, &ChildOf)>()
        .iter(world)
        .find(|(strip, _)| strip.id() == EXT_WORKSPACE_ID)
        .map(|(_, child)| child.parent())
        .expect("external workspace should remain managed");
    assert_eq!(external_parent, main);
    let window_entity = find_window_entity(100, world);
    let window = world.get::<Window>(window_entity).unwrap();
    assert!(window.frame().min.y >= TEST_MENUBAR_HEIGHT);
}

#[test]
fn test_late_space_rehome_reapplies_window_frame() {
    let mut harness = TestHarness::new().with_display(
        EXT_DISPLAY_ID,
        IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
        vec![EXT_WORKSPACE_ID],
    );
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    harness.mock_state.spawn_window(
        TEST_PROCESS_ID,
        EXT_WORKSPACE_ID,
        100,
        IRect::from_corners(
            ext_origin,
            ext_origin + Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT),
        ),
    );
    harness.advance(Duration::from_millis(500));
    harness.mock_state.remove_display(EXT_DISPLAY_ID);
    harness
        .world()
        .write_message::<Event>(Event::DisplayRemoved {
            display_id: EXT_DISPLAY_ID,
        });
    harness.advance(Duration::from_millis(650));

    let world = harness.world();
    let orphan = world
        .query::<(&LayoutStrip, Option<&ChildOf>)>()
        .iter(world)
        .find(|(strip, _)| strip.id() == EXT_WORKSPACE_ID)
        .expect("external workspace should be retained");
    assert!(orphan.1.is_none(), "workspace should await its new display");

    harness.mock_state.add_display(
        TEST_DISPLAY_ID,
        IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
        vec![TEST_WORKSPACE_ID, EXT_WORKSPACE_ID],
    );
    harness.advance(Duration::from_millis(1200));

    let world = harness.world();
    let external_parent = world
        .query::<(&LayoutStrip, &ChildOf)>()
        .iter(world)
        .find(|(strip, _)| strip.id() == EXT_WORKSPACE_ID)
        .map(|(_, child)| child.parent())
        .expect("external workspace should be reparented");
    let main = world.query::<(&Display, Entity)>().single(world).unwrap().1;
    assert_eq!(external_parent, main);
    let window_entity = find_window_entity(100, world);
    assert!(world.get::<Window>(window_entity).unwrap().frame().min.y >= TEST_MENUBAR_HEIGHT);
}

#[test]
fn test_wake_reapplies_window_frame_without_geometry_change() {
    let mut harness = TestHarness::new().with_windows(1);
    harness.advance(Duration::from_millis(500));
    let window_entity = find_window_entity(0, harness.world());
    let original = harness
        .world()
        .get::<Window>(window_entity)
        .unwrap()
        .frame();

    harness.mock_state.update_window(0, |window| {
        window.frame.min.x += 200;
        window.frame.max.x += 200;
    });
    harness
        .world()
        .write_message::<Event>(Event::SystemWoke { msg: String::new() });
    harness.advance(Duration::from_secs(1));

    let restored = harness
        .world()
        .get::<Window>(window_entity)
        .unwrap()
        .frame();
    assert_eq!(restored, original);
}

#[test]
fn test_vertical_swap_within_stack_stays_on_display() {
    // Regression test: with a display arranged *below* the active one, a
    // `Swap(South)` inside a stack used to swap the two windows and then
    // immediately send the focused one to the display below, because the
    // "is there anything left to swap with?" check ran against the layout
    // after the swap had already happened.
    let mut harness = TestHarness::new().with_windows(2);
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(
            0,
            TEST_DISPLAY_HEIGHT,
            EXT_DISPLAY_WIDTH,
            TEST_DISPLAY_HEIGHT + EXT_DISPLAY_HEIGHT,
        ),
        vec![EXT_WORKSPACE_ID],
    );

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::Window(Operation::Stack(true)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::North)),
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::Swap(Direction::South)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness
        .on_iteration(4, |world, state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
            assert_on_workspace!(world, 1, TEST_WORKSPACE_ID);
            assert_eq!(state.active_display(), TEST_DISPLAY_ID);
        })
        .on_iteration(6, |world, state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
            assert_on_workspace!(world, 1, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 0, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 1, EXT_WORKSPACE_ID);
            assert_eq!(
                state.active_display(),
                TEST_DISPLAY_ID,
                "swapping inside a stack must not move focus to another display"
            );
        })
        .run(commands);
}

#[test]
fn test_hidden_stack_stays_off_the_display_below() {
    // Regression test: hiding a virtual workspace parks its strip at the
    // display's bottom-right corner. Windows below the strip origin - the
    // lower members of a stack - used to land past the bottom edge entirely,
    // inside the display underneath, which macOS then adopts them onto. The
    // window came back on the wrong display once the workspace was shown
    // again.
    let mut harness = TestHarness::new().with_windows(2);
    harness.mock_state.add_display(
        EXT_DISPLAY_ID,
        IRect::new(
            0,
            TEST_DISPLAY_HEIGHT,
            EXT_DISPLAY_WIDTH,
            TEST_DISPLAY_HEIGHT + EXT_DISPLAY_HEIGHT,
        ),
        vec![EXT_WORKSPACE_ID],
    );

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::Last)),
        },
        Event::Command {
            command: Command::Window(Operation::Stack(true)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualNumber(1)),
        },
        Event::Command {
            command: Command::PrintState,
        },
        Event::Command {
            command: Command::Window(Operation::VirtualNumber(0)),
        },
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness
        .on_iteration(4, |world, _state| {
            // Hidden, but still parked on their own display: every window keeps
            // its origin above the top edge of the display below.
            let mut query = world.query::<&crate::manager::Window>();
            for window in query.iter(world) {
                let frame = window.frame();
                assert!(
                    frame.min.y < TEST_DISPLAY_HEIGHT,
                    "window {} parked at {:?}, inside the display below",
                    window.id(),
                    frame
                );
                assert_eq!(
                    frame.min.y,
                    TEST_DISPLAY_HEIGHT - PARKED_STRIP_SLIVER,
                    "window {} should park on the corner sliver",
                    window.id()
                );
            }
        })
        .on_iteration(6, |world, _state| {
            assert_on_workspace!(world, 0, TEST_WORKSPACE_ID);
            assert_on_workspace!(world, 1, TEST_WORKSPACE_ID);
            assert_not_on_workspace!(world, 0, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 1, EXT_WORKSPACE_ID);
            assert_window_at!(world, 0, 0, TEST_MENUBAR_HEIGHT);
            assert_window_at!(world, 1, 0, 394);
        })
        .run(commands);
}

/// Focusing a window on another display and coming back must not re-derive the
/// strip offset on the display we left: the centering the user asked for there
/// is still what they want to see when they return.
#[test]
fn test_center_survives_display_round_trip() {
    let config: Config = (
        MainOptions {
            auto_center: Some(false),
            continuous_swipe: Some(false),
            animation_speed: Some(10000.0),
            ..Default::default()
        },
        vec![],
    )
        .into();

    let centered = (TEST_DISPLAY_WIDTH - TEST_WINDOW_WIDTH) / 2;
    let window_x = |world: &mut World, id: WinID| -> i32 {
        let mut query = world.query::<&Window>();
        query
            .iter(world)
            .find(|window| window.id() == id)
            .expect("window not found")
            .frame()
            .min
            .x
    };

    let commands = vec![
        // 0: boot with focus on window 0.
        Event::MenuOpened { window_id: 0 },
        // 1: center it on the main display.
        Event::Command {
            command: Command::Window(Operation::Center),
        },
        // 2: focus moves to the window on the external display.
        Event::Command {
            command: Command::PrintState,
        },
        // 3: and back to window 0.
        Event::Command {
            command: Command::PrintState,
        },
    ];

    TestHarness::new()
        .with_config(config)
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(4)
        .with_workspace_window(100, EXT_WORKSPACE_ID, |window| {
            window.workspace_id = EXT_WORKSPACE_ID;
        })
        .on_iteration(1, move |world, state| {
            assert_eq!(window_x(world, 0), centered, "window 0 must be centered");
            state.focus_window(100);
        })
        .on_iteration(2, move |_world, state| {
            state.focus_window(0);
        })
        .on_iteration(3, move |world, _state| {
            assert_eq!(
                window_x(world, 0),
                centered,
                "returning from another display must not undo the centering"
            );
        })
        .run(commands);
}

#[test]
fn focusing_another_display_without_switching_its_workspace_does_not_flash() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(100, EXT_WORKSPACE_ID, |window| {
            window.workspace_id = EXT_WORKSPACE_ID;
        });
    harness.run(vec![Event::MenuOpened { window_id: 0 }]);

    // The focus event can arrive before the active-display notification.
    harness.mock_state.focus_window(100);
    harness.advance(Duration::from_millis(60));
    let world = harness.world();
    let mut active =
        world.query_filtered::<&LayoutStrip, With<crate::ecs::ActiveWorkspaceMarker>>();
    assert_eq!(active.single(world).unwrap().id(), EXT_WORKSPACE_ID);
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).next().is_none(),
        "focus on another display must not flash on the original display"
    );

    harness.mock_state.activate_display(EXT_DISPLAY_ID);
    harness.advance(Duration::from_millis(60));
    let world = harness.world();
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).next().is_none(),
        "recognizing the new display must not turn focus into a workspace popup"
    );

    harness.mock_state.focus_window(0);
    harness.advance(Duration::from_millis(60));
    let world = harness.world();
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).next().is_none(),
        "returning to an unchanged strip must also stay quiet"
    );

    harness.mock_state.activate_display(TEST_DISPLAY_ID);
    harness.advance(Duration::from_millis(60));
    let world = harness.world();
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(messages.iter(world).next().is_none());
}

#[test]
fn focus_south_to_another_display_does_not_flash() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(
                0,
                TEST_DISPLAY_HEIGHT,
                EXT_DISPLAY_WIDTH,
                TEST_DISPLAY_HEIGHT + EXT_DISPLAY_HEIGHT,
            ),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(100, EXT_WORKSPACE_ID, |window| {
            window.workspace_id = EXT_WORKSPACE_ID;
        });
    harness.run(vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::South)),
        },
    ]);

    let world = harness.world();
    let mut active =
        world.query_filtered::<&LayoutStrip, With<crate::ecs::ActiveWorkspaceMarker>>();
    assert_eq!(active.single(world).unwrap().id(), EXT_WORKSPACE_ID);
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).next().is_none(),
        "focus_south across displays must not create a workspace popup"
    );
}

#[test]
fn popup_waits_until_the_destination_display_is_active() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(100, EXT_WORKSPACE_ID, |window| {
            window.workspace_id = EXT_WORKSPACE_ID;
        });
    harness.run(vec![Event::MenuOpened { window_id: 0 }]);

    let target = {
        let world = harness.world();
        let display = world
            .query::<(Entity, &Display)>()
            .iter(world)
            .find_map(|(entity, display)| (display.id() == EXT_DISPLAY_ID).then_some(entity))
            .expect("external display");
        world
            .spawn((
                LayoutStrip::new(EXT_WORKSPACE_ID, 1),
                crate::ecs::Position(Origin::new(0, -EXT_DISPLAY_HEIGHT)),
                ChildOf(display),
            ))
            .id()
    };

    // Workspace activation arrives while the original display is still marked active.
    harness
        .world()
        .entity_mut(target)
        .insert(crate::ecs::ActiveWorkspaceMarker);
    harness.advance(Duration::from_millis(20));
    let world = harness.world();
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).next().is_none(),
        "the popup must not render on the old display"
    );

    harness.mock_state.activate_display(EXT_DISPLAY_ID);
    harness.advance(Duration::from_millis(60));
    let world = harness.world();
    let mut messages = world.query::<&crate::ecs::FlashMessage>();
    assert!(
        messages.iter(world).any(|message| message.0 == "2"),
        "the pending workspace change should flash after display recognition"
    );
}

/// An empty row 0 must survive its display going away. Despawning it left the
/// space renumbered from "2" — the menu bar lists only the rows that exist —
/// with no switch or reap path that recreates row 0.
#[test]
fn test_empty_baseline_row_survives_display_removal() {
    let mut commands = vec![
        Event::Command {
            command: Command::PrintState,
        },
        Event::DisplayRemoved {
            display_id: TEST_DISPLAY_ID,
        },
    ];
    commands.extend((0..6).map(|_| Event::Command {
        command: Command::PrintState,
    }));
    commands.push(Event::DisplayAdded {
        display_id: TEST_DISPLAY_ID,
    });

    TestHarness::new()
        .on_iteration(0, |world, state| {
            let strips = world
                .query::<&LayoutStrip>()
                .iter(world)
                .map(|strip| strip.virtual_index)
                .collect::<Vec<_>>();
            assert_eq!(strips, vec![0], "the space starts with an empty row 0");
            state.remove_display(TEST_DISPLAY_ID);
        })
        .on_iteration(7, |world, mut state| {
            let entity = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .single(world)
                .expect("empty row 0 should be orphaned, not despawned");
            assert!(
                world.entity(entity).get::<Timeout>().is_some(),
                "orphaned row 0 should carry a timeout"
            );
            state.add_display(
                TEST_DISPLAY_ID,
                IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT),
                vec![TEST_WORKSPACE_ID],
            );
        })
        .on_iteration(8, |world, _state| {
            let entity = world
                .query_filtered::<Entity, With<LayoutStrip>>()
                .single(world)
                .expect("row 0 should still exist after the display returns");
            assert!(
                world.entity(entity).get::<ChildOf>().is_some(),
                "row 0 should be re-parented to the returning display"
            );
        })
        .run(commands);
}

/// The window ids of each column of the strip for `workspace_id`, top window
/// of each column.
fn strip_columns(world: &mut World, workspace_id: u64) -> Vec<WinID> {
    let columns = world
        .query::<&LayoutStrip>()
        .iter(world)
        .find(|strip| strip.id() == workspace_id)
        .expect("strip should exist")
        .all_columns();
    columns
        .into_iter()
        .map(|entity| world.get::<Window>(entity).expect("window").id())
        .collect()
}

/// The laptop with the Dell above it and window 100 on the laptop, plus
/// `ext_windows` on the Dell.
fn harness_with_display_above(ext_windows: &[WinID]) -> TestHarness {
    let size = Size::new(TEST_WINDOW_WIDTH, TEST_WINDOW_HEIGHT);
    let ext_origin = Origin::new(0, -EXT_DISPLAY_HEIGHT + TEST_MENUBAR_HEIGHT);
    let ext_frame = IRect::from_corners(ext_origin, ext_origin + size);
    ext_windows.iter().fold(
        TestHarness::new()
            .with_display(
                EXT_DISPLAY_ID,
                IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
                vec![EXT_WORKSPACE_ID],
            )
            .with_window(100, |data| data.pid = TEST_PROCESS_ID),
        |harness, id| {
            harness.with_workspace_window(*id, EXT_WORKSPACE_ID, |data| data.frame = ext_frame)
        },
    )
}

fn to_display(direction: Direction) -> Event {
    Event::Command {
        command: Command::Window(Operation::ToDisplay(direction)),
    }
}

#[test]
fn test_display_north_moves_window_up() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        to_display(Direction::North),
    ];

    harness_with_display_above(&[])
        .on_iteration(1, |world, _state| {
            assert_on_workspace!(world, 100, EXT_WORKSPACE_ID);
            assert_not_on_workspace!(world, 100, TEST_WORKSPACE_ID);
            assert_focused!(world, 100);
        })
        .run(commands);
}

#[test]
fn test_display_south_without_display_below_is_noop() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        to_display(Direction::South),
    ];

    harness_with_display_above(&[])
        .with_window(101, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(0, |world, _state| {
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![100, 101]);
        })
        .on_iteration(1, |world, _state| {
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![100, 101]);
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), Vec::<WinID>::new());
            assert_focused!(world, 100);
        })
        .run(commands);
}

#[test]
fn test_display_move_lands_right_of_active_column() {
    let print = || Event::Command {
        command: Command::PrintState,
    };
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        print(),
        print(),
        to_display(Direction::North),
    ];

    // Visit 201 on the Dell so it becomes that row's active column, then
    // come back to the laptop.
    harness_with_display_above(&[200, 201, 202])
        .on_iteration(0, |_world, state| state.focus_window(201))
        .on_iteration(1, |_world, state| state.focus_window(100))
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 100);
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), vec![200, 201, 202]);
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(
                strip_columns(world, EXT_WORKSPACE_ID),
                vec![200, 201, 100, 202]
            );
            assert_focused!(world, 100);
        })
        .run(commands);
}

#[test]
fn test_display_move_into_empty_row() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        to_display(Direction::North),
    ];

    harness_with_display_above(&[])
        .with_window(101, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(1, |world, _state| {
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), vec![100]);
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![101]);
        })
        .run(commands);
}

#[test]
fn test_display_move_takes_tab_group() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::PrintState,
        },
        to_display(Direction::North),
    ];

    harness_with_display_above(&[])
        .on_iteration(0, |world, state| {
            super::tabs::spawn_matching_native_tab(world, &state, 100);
        })
        .on_iteration(1, |world, _state| {
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID).len(), 1);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), Vec::<WinID>::new());
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID).len(), 1);
            let (leader, follower) = (
                find_window_entity(100, world),
                find_window_entity(101, world),
            );
            let strip = world
                .query::<&LayoutStrip>()
                .iter(world)
                .find(|strip| strip.id() == EXT_WORKSPACE_ID)
                .expect("strip should exist");
            assert!(strip.tabbed(leader) && strip.tabbed(follower));
        })
        .run(commands);
}

#[test]
fn test_display_move_ignores_floating() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::Command {
            command: Command::Window(Operation::Manage),
        },
        to_display(Direction::North),
    ];

    harness_with_display_above(&[])
        .with_window(101, |data| data.pid = TEST_PROCESS_ID)
        .on_iteration(2, |world, _state| {
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), Vec::<WinID>::new());
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![101]);
        })
        .run(commands);
}

#[test]
fn test_fix_window_size_on_focus_only_after_display_change() {
    use crate::ecs::{Bounds, VerifyWindowSize};

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::DisplayChanged,
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::West)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
    ];

    TestHarness::new()
        .with_windows(2)
        .on_iteration(0, |_world, state| {
            // Silently change window 1's OS width without emitting WindowResized.
            state.update_window(1, |window| {
                window.frame.max.x = window.frame.min.x + 600;
            });
        })
        .on_iteration(1, |world, _state| {
            let win1 = find_window_entity(1, world);
            assert_eq!(
                world.get::<Bounds>(win1).expect("win1 bounds").0.x,
                TEST_WINDOW_WIDTH,
                "ordinary focus changes must not poll AX to re-read window size"
            );
            assert!(world.get::<VerifyWindowSize>(win1).is_none());
        })
        .on_iteration(2, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            assert!(
                world.get::<VerifyWindowSize>(win0).is_some()
                    && world.get::<VerifyWindowSize>(win1).is_some(),
                "DisplayChanged only attaches VerifyWindowSize without immediately polling AX"
            );
            assert_eq!(
                world.get::<Bounds>(win1).expect("win1 bounds").0.x,
                TEST_WINDOW_WIDTH,
                "already-focused window is not re-read until it next gains focus"
            );
        })
        .on_iteration(3, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            assert!(
                world.get::<VerifyWindowSize>(win0).is_none(),
                "focusing window 0 consumes its VerifyWindowSize marker"
            );
            assert!(
                world.get::<VerifyWindowSize>(win1).is_some(),
                "window 1 keeps VerifyWindowSize until it gains focus"
            );
        })
        .on_iteration(4, |world, _state| {
            let win1 = find_window_entity(1, world);
            assert_eq!(
                world.get::<Bounds>(win1).expect("win1 bounds").0.x,
                600,
                "focusing window 1 with VerifyWindowSize refreshes its Bounds from AX"
            );
            assert!(
                world.get::<VerifyWindowSize>(win1).is_none(),
                "window 1's VerifyWindowSize marker is removed after checking"
            );
        })
        .run(commands);
}

#[test]
fn test_attach_display_moves_virtual_workspace_together() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::DisplayAdded {
            display_id: EXT_DISPLAY_ID,
        },
    ];

    let mut harness = TestHarness::new().with_windows(4);
    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    harness
        .on_iteration(5, |world, mut state| {
            // Before attaching EXT_DISPLAY_ID: [0, 1] on v0, [2, 3] on v1.
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            let win2 = find_window_entity(2, world);
            let win3 = find_window_entity(3, world);

            let v0 = world
                .query::<&LayoutStrip>()
                .iter(world)
                .find(|s| s.id() == TEST_WORKSPACE_ID && s.virtual_index == 0)
                .expect("v0 on TEST_WORKSPACE_ID");
            assert_eq!(v0.all_windows(), vec![win0, win1]);

            let v1 = world
                .query::<&LayoutStrip>()
                .iter(world)
                .find(|s| s.id() == TEST_WORKSPACE_ID && s.virtual_index == 1)
                .expect("v1 on TEST_WORKSPACE_ID");
            assert_eq!(v1.all_windows(), vec![win2, win3]);

            // Attach EXT_DISPLAY_ID and report both windows 2 and 3 on EXT_WORKSPACE_ID.
            state.add_display(
                EXT_DISPLAY_ID,
                IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
                vec![EXT_WORKSPACE_ID],
            );
            state.update_window(2, |w| w.workspace_id = EXT_WORKSPACE_ID);
            state.update_window(3, |w| w.workspace_id = EXT_WORKSPACE_ID);
        })
        .on_iteration(6, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            let win2 = find_window_entity(2, world);
            let win3 = find_window_entity(3, world);

            let main_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == TEST_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(
                main_strips,
                vec![(0, vec![win0, win1])],
                "source display should only have v0 with [0, 1] after v1 moved away"
            );

            let ext_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == EXT_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(
                ext_strips,
                vec![(0, vec![win2, win3])],
                "new display should receive [2, 3] together in its first virtual workspace v0"
            );
        })
        .run(commands);
}

#[test]
fn test_attach_display_only_moves_windows_reported_by_os() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::DisplayAdded {
            display_id: EXT_DISPLAY_ID,
        },
    ];

    let mut harness = TestHarness::new().with_windows(4);
    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    harness
        .on_iteration(5, |_world, mut state| {
            // Attach EXT_DISPLAY_ID, but macOS only reports window 2 (not 3) on EXT_WORKSPACE_ID.
            state.add_display(
                EXT_DISPLAY_ID,
                IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
                vec![EXT_WORKSPACE_ID],
            );
            state.update_window(2, |w| w.workspace_id = EXT_WORKSPACE_ID);
        })
        .on_iteration(6, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            let win2 = find_window_entity(2, world);
            let win3 = find_window_entity(3, world);

            let mut main_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == TEST_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            main_strips.sort_by_key(|(idx, _)| *idx);
            assert_eq!(
                main_strips,
                vec![(0, vec![win0, win1]), (1, vec![win3])],
                "window 3 was not reported on the new display and must stay on v1 of main display"
            );

            let ext_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == EXT_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(
                ext_strips,
                vec![(0, vec![win2])],
                "only window 2 should move to the new display"
            );
        })
        .run(commands);
}

#[test]
fn test_attach_display_compacts_emptied_first_virtual_workspace() {
    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::Command {
            command: Command::Window(Operation::Focus(Direction::East)),
        },
        Event::Command {
            command: Command::Window(Operation::VirtualMoveNumber(1, MoveFocus::Stay)),
        },
        Event::DisplayAdded {
            display_id: EXT_DISPLAY_ID,
        },
    ];

    let mut harness = TestHarness::new().with_windows(4);
    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    harness
        .on_iteration(5, |_world, mut state| {
            // Attach EXT_DISPLAY_ID and move [0, 1] from v0 of TEST_WORKSPACE_ID to EXT_WORKSPACE_ID.
            state.add_display(
                EXT_DISPLAY_ID,
                IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
                vec![EXT_WORKSPACE_ID],
            );
            state.update_window(0, |w| w.workspace_id = EXT_WORKSPACE_ID);
            state.update_window(1, |w| w.workspace_id = EXT_WORKSPACE_ID);
        })
        .on_iteration(6, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            let win2 = find_window_entity(2, world);
            let win3 = find_window_entity(3, world);

            let main_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == TEST_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(
                main_strips,
                vec![(0, vec![win2, win3])],
                "emptied v0 on source display must be removed and v1 compacted to v0"
            );

            let ext_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == EXT_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(ext_strips, vec![(0, vec![win0, win1])]);
        })
        .run(commands);
}

#[test]
fn test_remove_display_appends_strip_as_next_virtual_workspace() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_windows(2)
        .with_workspace_window(100, EXT_WORKSPACE_ID, |_| {})
        .with_workspace_window(101, EXT_WORKSPACE_ID, |_| {});

    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    let commands = vec![
        Event::MenuOpened { window_id: 0 },
        Event::DisplayRemoved {
            display_id: EXT_DISPLAY_ID,
        },
    ];

    harness
        .on_iteration(0, |_world, state| {
            // Unplug EXT_DISPLAY_ID; macOS moves windows 100 and 101 to TEST_WORKSPACE_ID.
            state.remove_display(EXT_DISPLAY_ID);
            state.update_window(100, |w| w.workspace_id = TEST_WORKSPACE_ID);
            state.update_window(101, |w| w.workspace_id = TEST_WORKSPACE_ID);
        })
        .on_iteration(1, |world, _state| {
            let win0 = find_window_entity(0, world);
            let win1 = find_window_entity(1, world);
            let win100 = find_window_entity(100, world);
            let win101 = find_window_entity(101, world);

            let mut main_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == TEST_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            main_strips.sort_by_key(|(idx, _)| *idx);
            assert_eq!(
                main_strips,
                vec![(0, vec![win0, win1]), (1, vec![win100, win101])],
                "windows from removed display must become the next virtual workspace (v1) on surviving display, not merged into v0"
            );

            let leftover_ext = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == EXT_WORKSPACE_ID)
                .count();
            assert_eq!(
                leftover_ext, 0,
                "emptied orphan strip from removed display should be despawned"
            );
        })
        .run(commands);
}

#[test]
fn test_display_move_keeps_width_ratio() {
    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        to_display(Direction::North),
        Event::Command {
            command: Command::PrintState,
        },
    ];

    harness_with_display_above(&[])
        .on_iteration(2, |world, _state| {
            let config = world.resource::<Config>().clone();
            let mut viewports = world.query::<(&Display, Option<&DockPosition>)>();
            let mut viewport = |id| {
                viewports
                    .iter(world)
                    .find(|(display, _)| display.id() == id)
                    .map(|(display, dock)| display.actual_display_bounds(dock, &config))
                    .expect("display")
            };
            let (source, target) = (viewport(TEST_DISPLAY_ID), viewport(EXT_DISPLAY_ID));
            let width = crate::util::round_px(
                f64::from(TEST_WINDOW_WIDTH) / f64::from(source.width())
                    * f64::from(target.width()),
            );
            assert_window_size!(world, 100, width, target.height());
        })
        .run(commands);
}

#[test]
fn test_remove_display_populates_empty_first_workspace_on_surviving_display() {
    let mut harness = TestHarness::new()
        .with_display(
            EXT_DISPLAY_ID,
            IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0),
            vec![EXT_WORKSPACE_ID],
        )
        .with_workspace_window(100, EXT_WORKSPACE_ID, |_| {})
        .with_workspace_window(101, EXT_WORKSPACE_ID, |_| {});

    harness
        .app
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            500,
        )));

    let commands = vec![
        Event::MenuOpened { window_id: 100 },
        Event::DisplayRemoved {
            display_id: EXT_DISPLAY_ID,
        },
    ];

    harness
        .on_iteration(0, |_world, state| {
            // TEST_WORKSPACE_ID has no windows (empty v0); EXT_DISPLAY_ID has [100, 101].
            state.remove_display(EXT_DISPLAY_ID);
            state.update_window(100, |w| w.workspace_id = TEST_WORKSPACE_ID);
            state.update_window(101, |w| w.workspace_id = TEST_WORKSPACE_ID);
        })
        .on_iteration(1, |world, _state| {
            let win100 = find_window_entity(100, world);
            let win101 = find_window_entity(101, world);

            let main_strips: Vec<(u32, Vec<Entity>)> = world
                .query::<&LayoutStrip>()
                .iter(world)
                .filter(|s| s.id() == TEST_WORKSPACE_ID)
                .map(|s| (s.virtual_index, s.all_windows()))
                .collect();
            assert_eq!(
                main_strips,
                vec![(0, vec![win100, win101])],
                "surviving display with empty v0 must have no empty first workspace after receiving strip from removed display"
            );
        })
        .run(commands);
}

/// A Space that no display lists any more (P1–P4).
const DEAD_WORKSPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 1;

fn main_bounds() -> IRect {
    IRect::new(0, 0, TEST_DISPLAY_WIDTH, TEST_DISPLAY_HEIGHT)
}

fn ext_bounds() -> IRect {
    IRect::new(0, -EXT_DISPLAY_HEIGHT, EXT_DISPLAY_WIDTH, 0)
}

/// Startup can't read the external display's Spaces. Once the read works, the
/// retry must give that display its row.
#[test]
fn startup_retries_a_display_whose_spaces_cannot_be_read() {
    let harness =
        TestHarness::new().with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID]);
    harness
        .mock_state
        .set_space_list_unreadable(EXT_DISPLAY_ID, true);
    harness
        .on_iteration(0, |world, state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            assert_eq!(row_parents(world, TEST_WORKSPACE_ID), [Some(main)]);
            assert!(
                row_parents(world, EXT_WORKSPACE_ID).is_empty(),
                "not read yet"
            );
            state.set_space_list_unreadable(EXT_DISPLAY_ID, false);
        })
        .on_iteration(3, |world, _state| {
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(
                row_parents(world, EXT_WORKSPACE_ID),
                [Some(ext)],
                "the retry gives the display its row"
            );
        })
        .run(idle(4));
}

/// Startup sees no display at all. Once macOS reports one, the retry must
/// build it with both active markers.
#[test]
fn startup_rebuilds_the_world_after_an_empty_display_scan() {
    let harness = TestHarness::new();
    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    harness
        .on_iteration(0, |world, mut state| {
            assert_eq!(world.query::<&Display>().iter(world).count(), 0);
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(3, |world, _state| {
            assert_eq!(
                world.query::<&Display>().iter(world).count(),
                1,
                "the retry spawns the display"
            );
            let main = display_entity(world, TEST_DISPLAY_ID);
            assert!(world.get::<ActiveDisplayMarker>(main).is_some());
            assert_eq!(row_parents(world, TEST_WORKSPACE_ID), [Some(main)]);
            assert_eq!(active_space(world), Some(TEST_WORKSPACE_ID));
        })
        .run(idle(4));
}

/// The active Space can't be read at startup (the active display id names no
/// display). Every display must still be scanned, and the retry sets the
/// active markers once the read works.
#[test]
fn startup_keeps_scanning_displays_when_the_active_space_cannot_be_read() {
    let harness =
        TestHarness::new().with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID]);
    harness.mock_state.set_active_display(999);
    harness
        .on_iteration(0, |world, state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(row_parents(world, TEST_WORKSPACE_ID), [Some(main)]);
            assert_eq!(row_parents(world, EXT_WORKSPACE_ID), [Some(ext)]);
            state.set_active_display(TEST_DISPLAY_ID);
        })
        .on_iteration(3, |world, _state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            assert!(world.get::<ActiveDisplayMarker>(main).is_some());
            assert_eq!(active_space(world), Some(TEST_WORKSPACE_ID));
        })
        .run(idle(4));
}

/// Reconcile's retries run out while a display's Space list stays unreadable.
/// Applying that scan must leave the display and its rows alone.
#[test]
fn reconcile_keeps_rows_of_a_display_whose_spaces_cannot_be_read() {
    let first_ext = std::rc::Rc::new(std::cell::Cell::new(None));
    let saved_ext = first_ext.clone();
    let mut commands = idle(11);
    commands[1] = Event::DisplayConfigured {
        display_id: EXT_DISPLAY_ID,
    };

    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID])
        .on_iteration(0, move |world, state| {
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(row_parents(world, EXT_WORKSPACE_ID), [Some(ext)]);
            saved_ext.set(Some(ext));
            state.set_space_list_unreadable(EXT_DISPLAY_ID, true);
        })
        .on_iteration(10, move |world, _state| {
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(Some(ext), first_ext.get(), "the display is the same entity");
            assert_eq!(row_parents(world, EXT_WORKSPACE_ID), [Some(ext)]);
            assert!(
                !world
                    .query::<(&LayoutStrip, Has<Timeout>)>()
                    .iter(world)
                    .any(|(strip, timeout)| strip.id() == EXT_WORKSPACE_ID && timeout),
                "the row is not orphaned"
            );
        })
        .run(commands);
}

/// A Space gone from a complete, consistent scan loses its rows.
#[test]
fn prune_removes_rows_of_a_space_no_display_lists() {
    TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            main_bounds(),
            vec![TEST_WORKSPACE_ID, DEAD_WORKSPACE_ID],
        )
        .on_iteration(0, |world, mut state| {
            assert!(!row_parents(world, DEAD_WORKSPACE_ID).is_empty());
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(5, |world, _state| {
            assert!(
                row_parents(world, DEAD_WORKSPACE_ID).is_empty(),
                "the dead Space's row is pruned"
            );
            assert_eq!(active_space(world), Some(TEST_WORKSPACE_ID));
        })
        .run(idle(6));
}

/// A scan with an unreadable display is partial: nothing is pruned.
#[test]
fn prune_keeps_rows_while_a_display_cannot_be_read() {
    TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            main_bounds(),
            vec![TEST_WORKSPACE_ID, DEAD_WORKSPACE_ID],
        )
        .with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID])
        .on_iteration(0, |_world, mut state| {
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
            state.set_space_list_unreadable(EXT_DISPLAY_ID, true);
        })
        .on_iteration(5, |world, _state| {
            assert!(!row_parents(world, DEAD_WORKSPACE_ID).is_empty());
            assert!(!row_parents(world, EXT_WORKSPACE_ID).is_empty());
        })
        .run(idle(6));
}

/// A scan whose display set differs from the world's is inconsistent (a
/// display event still in flight): nothing is pruned.
#[test]
fn prune_keeps_rows_while_the_display_set_differs() {
    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID])
        .on_iteration(0, |_world, state| state.remove_display(EXT_DISPLAY_ID))
        .on_iteration(5, |world, _state| {
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(row_parents(world, EXT_WORKSPACE_ID), [Some(ext)]);
        })
        .run(idle(6));
}

/// Two displays listing the same Space (the "Main" identifier mix-up) make
/// the scan inconsistent: nothing is pruned.
#[test]
fn prune_keeps_rows_when_two_displays_list_the_same_space() {
    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID])
        .on_iteration(0, |_world, mut state| {
            state.add_display(EXT_DISPLAY_ID, ext_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(5, |world, _state| {
            assert!(!row_parents(world, EXT_WORKSPACE_ID).is_empty());
        })
        .run(idle(6));
}

/// A display stays unreadable past reconcile's retries, so it is left with no
/// rows. Once a scan reads it, the prune pass must ask reconcile to rebuild it.
#[test]
fn heal_rebuilds_a_display_unreadable_past_reconciles_retries() {
    let harness =
        TestHarness::new().with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID]);
    harness
        .mock_state
        .set_space_list_unreadable(EXT_DISPLAY_ID, true);
    harness
        .on_iteration(10, |world, state| {
            assert!(!reconcile_pending(world), "reconcile has given up");
            assert!(row_parents(world, EXT_WORKSPACE_ID).is_empty());
            state.set_space_list_unreadable(EXT_DISPLAY_ID, false);
        })
        .on_iteration(14, |world, _state| {
            let ext = display_entity(world, EXT_DISPLAY_ID);
            assert_eq!(
                row_parents(world, EXT_WORKSPACE_ID),
                [Some(ext)],
                "the heal gives the display its row"
            );
        })
        .run(idle(15));
}

/// macOS reports no display past reconcile's retries, so the world stays
/// empty. Once a display shows up, the prune pass must ask reconcile to build it.
#[test]
fn heal_rebuilds_a_world_left_empty_past_reconciles_retries() {
    let harness = TestHarness::new();
    harness.mock_state.remove_display(TEST_DISPLAY_ID);
    harness
        .on_iteration(10, |world, mut state| {
            assert!(!reconcile_pending(world), "reconcile has given up");
            assert_eq!(world.query::<&Display>().iter(world).count(), 0);
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(14, |world, _state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            assert!(world.get::<ActiveDisplayMarker>(main).is_some());
            assert_eq!(row_parents(world, TEST_WORKSPACE_ID), [Some(main)]);
            assert_eq!(active_space(world), Some(TEST_WORKSPACE_ID));
        })
        .run(idle(15));
}

/// A native fullscreen Space (P4 tests).
const FULLSCREEN_SPACE_ID: WorkspaceId = TEST_WORKSPACE_ID + 100;

/// Removing a desktop despawns its rows with the windows in them. The windows
/// macOS moves to another Space must tile into that Space's shown row, but
/// only once they have been stray on two passes in a row.
#[test]
fn adopt_tiles_the_windows_of_a_removed_desktop() {
    let mut commands = idle(5);
    commands[1] = Event::SpaceDestroyed {
        space_id: DEAD_WORKSPACE_ID,
    };

    TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            main_bounds(),
            vec![TEST_WORKSPACE_ID, DEAD_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(1, DEAD_WORKSPACE_ID, |_| {})
        .with_workspace_window(2, DEAD_WORKSPACE_ID, |_| {})
        .on_iteration(0, |world, mut state| {
            let windows = [1, 2].map(|id| find_window_entity(id, world));
            let dead_row = row_entity(world, DEAD_WORKSPACE_ID, 0);
            assert_eq!(row_windows(world, dead_row), windows);
            // As macOS does: the windows of a removed desktop go to another Space.
            for id in [1, 2] {
                state.update_window(id, |window| window.workspace_id = TEST_WORKSPACE_ID);
            }
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(2, |world, _state| {
            assert!(
                !in_a_row(world, 1) && !in_a_row(world, 2),
                "one pass is not enough to adopt a window"
            );
        })
        .on_iteration(4, |world, _state| {
            let home = row_entity(world, TEST_WORKSPACE_ID, 0);
            let tiled = row_windows(world, home);
            assert_eq!(tiled.len(), 3, "row 0 holds {tiled:?}");
            for id in [0, 1, 2] {
                let window = find_window_entity(id, world);
                assert!(tiled.contains(&window), "window {id} is adopted into row 0");
            }
            assert!(row_parents(world, DEAD_WORKSPACE_ID).is_empty());
        })
        .run(commands);
}

/// A window just tiled where a binding was pressed is left alone while its
/// move settles (the 6bf2ea4 race), even if its row goes away: it is adopted
/// only once settled.
#[test]
fn adopt_waits_for_a_launched_window_to_settle() {
    let mut commands = idle(11);
    commands[1] = Event::SpaceDestroyed {
        space_id: DEAD_WORKSPACE_ID,
    };

    TestHarness::new()
        .with_display(
            TEST_DISPLAY_ID,
            main_bounds(),
            vec![TEST_WORKSPACE_ID, DEAD_WORKSPACE_ID],
        )
        .with_windows(1)
        .with_workspace_window(1, DEAD_WORKSPACE_ID, |_| {})
        .with_workspace_window(2, DEAD_WORKSPACE_ID, |_| {})
        .on_iteration(0, |world, mut state| {
            let now = world.resource::<Time>().elapsed();
            let launched = find_window_entity(1, world);
            world
                .entity_mut(launched)
                .insert(crate::ecs::LaunchedInto(now));
            for id in [1, 2] {
                state.update_window(id, |window| window.workspace_id = TEST_WORKSPACE_ID);
            }
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(4, |world, _state| {
            assert!(in_a_row(world, 2), "a settled window is adopted");
            assert!(!in_a_row(world, 1), "a settling window is left alone");
        })
        .on_iteration(10, |world, _state| {
            assert!(in_a_row(world, 1), "adopted once settled");
        })
        .run(commands);
}

/// A window already fullscreen at startup gets a fullscreen row that knows its
/// home, and no spare row. Leaving fullscreen brings it back to that home.
#[test]
fn fullscreen_window_found_at_startup_returns_home_when_it_leaves_fullscreen() {
    let config: Config = (
        MainOptions {
            dynamic_workspaces: Some(true),
            ..Default::default()
        },
        vec![],
    )
        .into();
    let harness = TestHarness::new().with_config(config).with_display(
        TEST_DISPLAY_ID,
        main_bounds(),
        vec![TEST_WORKSPACE_ID, FULLSCREEN_SPACE_ID],
    );
    harness
        .mock_state
        .set_space_fullscreen(FULLSCREEN_SPACE_ID, true);
    harness
        .with_windows(1)
        .with_workspace_window(1, FULLSCREEN_SPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .on_iteration(0, |world, mut state| {
            let window = find_window_entity(1, world);
            let home = row_entity(world, TEST_WORKSPACE_ID, 0);
            let rows = fullscreen_rows(world, FULLSCREEN_SPACE_ID);
            assert_eq!(rows.len(), 1, "a fullscreen Space gets no spare row");
            let (is_fullscreen, windows, marker) = &rows[0];
            assert!(is_fullscreen, "the row is a fullscreen strip");
            assert_eq!(windows, &[window]);
            assert_eq!(*marker, Some((home, TEST_WORKSPACE_ID)));
            assert!(!row_windows(world, home).contains(&window));

            // The window leaves fullscreen: macOS moves it home and drops the Space.
            state.update_window(1, |window| {
                window.workspace_id = TEST_WORKSPACE_ID;
                window.is_full_screen = false;
            });
            state.add_display(TEST_DISPLAY_ID, main_bounds(), vec![TEST_WORKSPACE_ID]);
        })
        .on_iteration(1, |world, _state| {
            let window = find_window_entity(1, world);
            let home = row_entity(world, TEST_WORKSPACE_ID, 0);
            assert_eq!(row_windows(world, home).last(), Some(&window));
            assert!(row_parents(world, FULLSCREEN_SPACE_ID).is_empty());
        })
        .run(vec![
            Event::Command {
                command: Command::PrintState,
            },
            Event::SpaceDestroyed {
                space_id: FULLSCREEN_SPACE_ID,
            },
        ]);
}

/// A display whose only Space is fullscreen still has a row parented to it,
/// so the heal stays quiet and saves are not blocked.
#[test]
fn display_whose_only_space_is_fullscreen_keeps_a_row() {
    use bevy::ecs::system::RunSystemOnce as _;

    let check = |world: &mut World, _state: MockState| {
        let ext = display_entity(world, EXT_DISPLAY_ID);
        assert_eq!(row_parents(world, EXT_WORKSPACE_ID), [Some(ext)]);
        assert!(!reconcile_pending(world), "the heal never fires");
        let blocked = world
            .run_system_once(|guard: crate::ecs::state::SaveGuard| guard.blocked_by())
            .expect("guard runs");
        assert_eq!(blocked, None, "a save is allowed");
    };
    let harness =
        TestHarness::new().with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID]);
    harness
        .mock_state
        .set_space_fullscreen(EXT_WORKSPACE_ID, true);
    harness
        .with_workspace_window(100, EXT_WORKSPACE_ID, |window| {
            window.is_full_screen = true;
        })
        .on_iteration(0, check)
        .on_iteration(3, check)
        .on_iteration(5, check)
        .run(idle(6));
}

/// Spaces found after startup get what startup gives a Space: a floating
/// layer, and for a fullscreen Space a row that knows its home.
#[test]
fn spaces_found_after_startup_get_a_floating_layer_and_a_home() {
    const NEW: WorkspaceId = TEST_WORKSPACE_ID + 2;
    const LATE_FS: WorkspaceId = TEST_WORKSPACE_ID + 101;
    let mut commands = idle(4);
    commands[0] = Event::SpaceCreated { space_id: NEW };
    commands[1] = Event::DisplayConfigured {
        display_id: TEST_DISPLAY_ID,
    };

    TestHarness::new()
        .on_iteration(0, |world, mut state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            assert_eq!(floating_layer_parents(world, NEW), [main]);
            state.add_display(
                TEST_DISPLAY_ID,
                main_bounds(),
                vec![TEST_WORKSPACE_ID, NEW, LATE_FS],
            );
            state.set_space_fullscreen(LATE_FS, true);
        })
        .on_iteration(3, |world, _state| {
            let main = display_entity(world, TEST_DISPLAY_ID);
            let home = row_entity(world, TEST_WORKSPACE_ID, 0);
            assert_eq!(row_parents(world, LATE_FS), [Some(main)]);
            let markers = fullscreen_rows(world, LATE_FS)
                .into_iter()
                .map(|(_, _, marker)| marker)
                .collect::<Vec<_>>();
            assert_eq!(markers, [Some((home, TEST_WORKSPACE_ID))]);
            assert_eq!(floating_layer_parents(world, LATE_FS), [main]);
        })
        .run(commands);
}

/// The row of `space` at `virtual_index`.
fn row_entity(world: &mut World, space: WorkspaceId, virtual_index: u32) -> Entity {
    world
        .query::<(Entity, &LayoutStrip)>()
        .iter(world)
        .find(|(_, strip)| strip.id() == space && strip.virtual_index == virtual_index)
        .map_or_else(
            || panic!("row {virtual_index} of space {space} not found"),
            |(entity, _)| entity,
        )
}

fn row_windows(world: &World, row: Entity) -> Vec<Entity> {
    world
        .get::<LayoutStrip>(row)
        .expect("row exists")
        .all_windows()
}

fn in_a_row(world: &mut World, id: WinID) -> bool {
    let window = find_window_entity(id, world);
    world
        .query::<&LayoutStrip>()
        .iter(world)
        .any(|strip| strip.contains(window))
}

/// A row: whether it is a fullscreen strip, its windows, and its fullscreen
/// marker's home row and Space.
type RowSummary = (bool, Vec<Entity>, Option<(Entity, WorkspaceId)>);

/// Every row of `space`.
fn fullscreen_rows(world: &mut World, space: WorkspaceId) -> Vec<RowSummary> {
    world
        .query::<(&LayoutStrip, Option<&NativeFullscreenMarker>)>()
        .iter(world)
        .filter(|(strip, _)| strip.id() == space)
        .map(|(strip, marker)| {
            (
                strip.is_fullscreen(),
                strip.all_windows(),
                marker.map(|marker| (marker.layout_strip, marker.workspace_id)),
            )
        })
        .collect()
}

fn floating_layer_parents(world: &mut World, space: WorkspaceId) -> Vec<Entity> {
    world
        .query::<(&FloatingLayer, &ChildOf)>()
        .iter(world)
        .filter(|(layer, _)| layer.workspace_id == space)
        .map(|(_, child)| child.parent())
        .collect()
}

fn reconcile_pending(world: &World) -> bool {
    world
        .resource::<crate::ecs::display::DisplayReconcileState>()
        .is_pending()
}

/// The parent of every row of `space`: `Some(display)`, or `None` for an orphan.
fn row_parents(world: &mut World, space: WorkspaceId) -> Vec<Option<Entity>> {
    world
        .query::<(&LayoutStrip, Option<&ChildOf>)>()
        .iter(world)
        .filter(|(strip, _)| strip.id() == space)
        .map(|(_, child)| child.map(ChildOf::parent))
        .collect()
}

fn display_entity(world: &mut World, id: u32) -> Entity {
    world
        .query::<(&Display, Entity)>()
        .iter(world)
        .find(|(display, _)| display.id() == id)
        .map_or_else(|| panic!("display {id} not found"), |(_, entity)| entity)
}

/// The Space of the row marked active, if exactly one is.
fn active_space(world: &mut World) -> Option<WorkspaceId> {
    world
        .query_filtered::<&LayoutStrip, With<ActiveWorkspaceMarker>>()
        .single(world)
        .ok()
        .map(LayoutStrip::id)
}

/// The laptop's three windows, with the empty Dell above it.
fn three_windows_below_an_empty_display() -> TestHarness {
    TestHarness::new()
        .with_display(EXT_DISPLAY_ID, ext_bounds(), vec![EXT_WORKSPACE_ID])
        .with_windows(3)
}

fn to_next_display(move_focus: MoveFocus) -> Event {
    Event::Command {
        command: Command::Window(Operation::ToNextDisplay(move_focus)),
    }
}

/// Sending the focused middle column to the other display focuses the column
/// that slides into its place, like closing it does.
#[test]
fn test_send_middle_window_to_next_display_focuses_its_right_neighbour() {
    let mut commands = idle(2);
    commands.push(to_next_display(MoveFocus::Stay));

    three_windows_below_an_empty_display()
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| {
            assert_focused!(world, 1);
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![0, 1, 2]);
        })
        .on_iteration(2, |world, _state| {
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), vec![1]);
            assert_eq!(strip_columns(world, TEST_WORKSPACE_ID), vec![0, 2]);
            assert_focused!(world, 2);
        })
        .run(commands);
}

/// Following the middle column to the other display: the display it left
/// remembers the column that slid into its place, for when focus comes back.
#[test]
fn test_follow_middle_window_to_next_display_leaves_its_successor_remembered() {
    let mut commands = idle(2);
    commands.push(to_next_display(MoveFocus::Follow));

    three_windows_below_an_empty_display()
        .on_iteration(0, |_world, state| state.focus_window(1))
        .on_iteration(1, |world, _state| assert_focused!(world, 1))
        .on_iteration(2, |world, _state| {
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), vec![1]);
            assert_focused!(world, 1);
            let right = find_window_entity(2, world);
            assert_eq!(
                world
                    .resource::<FocusHistory>()
                    .last_managed(TEST_WORKSPACE_ID),
                Some(right)
            );
        })
        .run(commands);
}

/// Focusing another display goes back to the window last used there, not to
/// the widest one, while any of it shows.
#[test]
fn test_mouse_to_display_lands_on_the_window_last_used_there() {
    let mut commands = idle(3);
    commands.push(Event::Command {
        command: Command::Mouse(MouseMove::ToDisplay(Direction::North)),
    });

    harness_with_display_above(&[200, 201])
        .on_iteration(0, |_world, state| state.focus_window(201))
        .on_iteration(1, |world, state| {
            assert_focused!(world, 201);
            state.focus_window(100);
        })
        .on_iteration(2, |world, _state| {
            assert_focused!(world, 100);
            assert_eq!(strip_columns(world, EXT_WORKSPACE_ID), vec![200, 201]);
            for id in [200, 201] {
                let entity = find_window_entity(id, world);
                let frame = world.get::<Window>(entity).expect("window").frame();
                assert_eq!(ext_bounds().intersect(frame), frame, "{id} fully shown");
            }
        })
        .on_iteration(3, |world, _state| assert_focused!(world, 201))
        .run(commands);
}
