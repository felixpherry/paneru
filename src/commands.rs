use bevy::app::{PostUpdate, PreUpdate};
use bevy::ecs::entity::{Entity, EntityHashSet};
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{Has, With};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::schedule::common_conditions::{on_message, resource_changed};
use bevy::ecs::system::{Commands, Query, Res, ResMut, Single};
use bevy::math::IRect;
use bevy::time::Time;
use tracing::{Level, instrument};
use tracing::{debug, error, info, warn};

mod query;

use crate::config::Config;
use crate::config::snippet::{RuleSubject, SnippetDialect, window_rule_snippet};
use crate::ecs::display::FloatingLayer;
use crate::ecs::focus::{FocusHistory, leave_row};
use crate::ecs::layout::{
    Column, LayoutStrip, MIN_WINDOW_HEIGHT, StackItem, clamp_origin_to_viewport, strip_signature,
};
use crate::ecs::params::{ActiveDisplay, ActiveDisplayMut, Windows};
use crate::ecs::tiling::Tiling;
use crate::ecs::workspace::{PreviousStripPosition, column_closest_to_center};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, DockPosition, FocusedMarker, FullWidthMarker,
    ManualStripOffset, NativeFullscreenMarker, OpenedBeside, RaiseWindow, SelectedVirtualMarker,
    SendMessageTrigger, SpawnCommandsExt, Timeout, Unmanaged,
};
use crate::events::Event;
use crate::manager::{Application, Display, Origin, Size, Window, WindowManager, origin_from};
use crate::platform::WorkspaceId;
use crate::util::round_px;

pub use crate::types::commands::{
    Command, Direction, MouseMove, MoveFocus, Operation, ResizeDirection,
};

/// Every row, with whether it is on screen, whether it is its display's
/// selected row, and whether it has a display.
type RowsWithVisibility<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut LayoutStrip,
        Has<ActiveWorkspaceMarker>,
        Has<SelectedVirtualMarker>,
        Has<ChildOf>,
    ),
>;

/// Every strip alongside the two flags that say where it sits: whether it is the
/// one currently on screen, and whether it is its display's selected virtual
/// workspace.
type StripsWithVisibility<'w, 's> = Query<
    'w,
    's,
    (
        &'static ChildOf,
        &'static LayoutStrip,
        Entity,
        Has<ActiveWorkspaceMarker>,
        Has<SelectedVirtualMarker>,
    ),
>;

pub fn register_commands(app: &mut bevy::app::App) {
    app.init_resource::<crate::menubar::MenuBarVisibility>()
        .add_systems(
            PreUpdate,
            crate::menubar::menu_bar_command_handler
                .in_set(crate::ecs::PreUpdateSet::Dispatch)
                .run_if(on_message::<Event>),
        )
        .add_systems(
            PostUpdate,
            crate::menubar::save_menu_bar_preference
                .in_set(crate::ecs::PostUpdateSet::Bookkeeping)
                .run_if(resource_changed::<crate::menubar::MenuBarVisibility>),
        );
    // Registered here (not with the Lua systems) so it's exercised by the mock
    // harness without a running interpreter.
    #[cfg(feature = "lua")]
    app.add_systems(
        PreUpdate,
        crate::ecs::layout_ops::apply_layout_ops.in_set(crate::ecs::PreUpdateSet::Dispatch),
    );

    query::register_query_commands(app);
    // Empty store so the mock harness and saveless runs still have one to
    // answer from; the real app overwrites it from disk.
    app.init_resource::<crate::ecs::script_state::ScriptStateStore>();
    app.add_systems(
        PreUpdate,
        crate::ecs::script_state::script_state_handler.in_set(crate::ecs::PreUpdateSet::Dispatch),
    );
    app.add_systems(
        PreUpdate,
        (
            command_quit_handler,
            command_restart_handler,
            print_internal_state_handler,
            mouse_to_next_display,
            resize_window,
            resize_window_vertical,
            command_center_window,
            full_width_window,
            to_next_display,
            equalize_column,
            balance_strip,
            manage_window,
            stack_windows_handler,
            command_move_focus,
            command_focus_unmanaged,
            command_focus_managed,
            command_raise_floating,
            command_toggle_floating_layer,
            command_swap_focus,
            snap_window,
        )
            .in_set(crate::ecs::PreUpdateSet::Dispatch),
    );
    // A separate registration because the tuple above is already at Bevy's
    // 20-system limit.
    //
    // A default dialect so the mock harness has one; the real app overwrites it
    // once it knows whether a Lua script took over the configuration.
    app.init_resource::<SnippetDialect>();
    app.add_systems(
        PreUpdate,
        (
            copy_window_rule,
            close_window,
            toggle_tabbed_display_handler,
            toggle_layout,
        )
            .in_set(crate::ecs::PreUpdateSet::Dispatch),
    );
}

pub fn filter_window_operations<'a, F: Fn(&Operation) -> bool>(
    messages: &'a mut MessageReader<Event>,
    filter: F,
) -> impl Iterator<Item = &'a Operation> {
    messages.read().filter_map(move |event| {
        if let Event::Command {
            command: Command::Window(op),
        } = event
            && filter(op)
        {
            Some(op)
        } else {
            None
        }
    })
}

/// Retrieves a window `Entity` in a specified direction relative to a `current_window_id` within a `LayoutStrip`.
///
/// # Arguments
///
/// * `direction` - The direction (e.g., `West`, `East`, `First`, `Last`, `North`, `South`).
/// * `current_window_id` - The `Entity` of the current window.
/// * `strip` - A reference to the `LayoutStrip` to search within.
///
/// # Returns
///
/// `Some(Entity)` with the found window's entity, otherwise `None`.
#[instrument(level = Level::DEBUG, ret)]
pub(crate) fn get_window_in_direction(
    direction: &Direction,
    entity: Entity,
    strip: &LayoutStrip,
) -> Option<Entity> {
    let index = strip.index_of(entity).ok()?;

    match direction {
        Direction::West => strip.left_neighbour(entity),
        Direction::East => strip.right_neighbour(entity),

        Direction::First => strip.first().ok().and_then(|column| column.top()),

        Direction::Last => strip.last().ok().and_then(|column| column.top()),

        Direction::Nth(index) => strip.get(*index).ok().and_then(|column| column.top()),

        Direction::North => match strip.get(index).ok()? {
            Column::Single(_) | Column::Tabs(_) | Column::Fullscren(_) => None,
            Column::Stack(stack) => stack
                .iter()
                .enumerate()
                .find(|(_, item)| item.contains(entity))
                .and_then(|(index, _)| (index > 0).then(|| stack.get(index - 1)).flatten())
                .and_then(StackItem::top),
        },

        Direction::South => match strip.get(index).ok()? {
            Column::Single(_) | Column::Tabs(_) | Column::Fullscren(_) => None,
            Column::Stack(stack) => stack
                .iter()
                .enumerate()
                .find(|(_, item)| item.contains(entity))
                .and_then(|(index, _)| {
                    (index < stack.len() - 1)
                        .then(|| stack.get(index + 1))
                        .flatten()
                })
                .and_then(StackItem::top),
        },
    }
}

/// 45° direction cone, closest by squared Euclidean distance.
/// `First` / `Last` are strip-only and return `None`.
fn pick_nearest_in_direction(
    direction: &Direction,
    focused_center: bevy::math::IVec2,
    candidates: impl IntoIterator<Item = (Entity, bevy::math::IVec2)>,
) -> Option<Entity> {
    candidates
        .into_iter()
        .filter_map(|(entity, center)| {
            let dx = center.x - focused_center.x;
            let dy = center.y - focused_center.y;
            let in_direction = match direction {
                Direction::East => dx > 0 && dy.abs() <= dx.abs(),
                Direction::West => dx < 0 && dy.abs() <= dx.abs(),
                Direction::North => dy < 0 && dx.abs() <= dy.abs(),
                Direction::South => dy > 0 && dx.abs() <= dy.abs(),
                Direction::First | Direction::Last | Direction::Nth(_) => return None,
            };
            in_direction.then_some((entity, dx * dx + dy * dy))
        })
        .min_by_key(|(_, dist_sq)| *dist_sq)
        .map(|(entity, _)| entity)
}

/// Where a directional move lands in `strip`: by column order on a scrolling
/// row; on a dwindle or master row, within a stacked tile first, then the
/// nearest tile on screen that way. `frame_of` gives strip-relative frames.
pub(crate) fn neighbour_in_direction<F>(
    direction: &Direction,
    entity: Entity,
    strip: &LayoutStrip,
    frame_of: F,
) -> Option<Entity>
where
    F: Fn(Entity) -> Option<IRect>,
{
    if !strip.is_tiled()
        || matches!(
            direction,
            Direction::First | Direction::Last | Direction::Nth(_)
        )
    {
        return get_window_in_direction(direction, entity, strip);
    }
    // ↑/↓ walk a stacked tile first. A tabbed stack's windows share one rect,
    // so the screen alone would never find the next tab.
    if matches!(direction, Direction::North | Direction::South)
        && let Some(sibling) = get_window_in_direction(direction, entity, strip)
    {
        return Some(sibling);
    }
    let index = strip.index_of(entity).ok()?;
    // One face per stack item of every other column, in column order, so a tie
    // goes to the earlier column (`min_by_key` keeps the first minimum).
    let faces = strip
        .columns()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .flat_map(|(_, column)| match column {
            Column::Stack(items) => items.iter().filter_map(StackItem::top).collect::<Vec<_>>(),
            column => column.top().into_iter().collect(),
        })
        .filter_map(|face| frame_of(face).map(|frame| (face, frame.center())));
    pick_nearest_in_direction(direction, frame_of(entity)?.center(), faces)
}

fn visible_floating_entities(
    windows: &Windows,
    window_manager: &WindowManager,
    workspace_id: WorkspaceId,
    display_bounds: IRect,
) -> Vec<Entity> {
    let workspace_window_ids: std::collections::HashSet<_> = window_manager
        .windows_in_workspace(workspace_id)
        .ok()
        .map(|ids| ids.into_iter().collect())
        .unwrap_or_default();

    windows
        .iter()
        .filter_map(|(_, entity)| {
            let (window, _, Some(Unmanaged::Floating)) = windows.get_managed(entity)? else {
                return None;
            };
            if !workspace_window_ids.contains(&window.id()) {
                return None;
            }
            let frame = windows.frame(entity)?;
            (!display_bounds.intersect(frame).is_empty()).then_some(entity)
        })
        .collect()
}

fn nearest_float_in_direction(
    direction: &Direction,
    focused_entity: Entity,
    windows: &Windows,
    window_manager: &WindowManager,
    workspace_id: WorkspaceId,
    display_bounds: IRect,
) -> Option<Entity> {
    let focused_center = windows.frame(focused_entity)?.center();

    let candidates =
        visible_floating_entities(windows, window_manager, workspace_id, display_bounds)
            .into_iter()
            .filter(|entity| *entity != focused_entity)
            .filter_map(|entity| windows.frame(entity).map(|frame| (entity, frame.center())));

    pick_nearest_in_direction(direction, focused_center, candidates)
}

/// Handles the "focus" command, moving focus to a window in a specified direction.
///
/// # Arguments
///
/// * `direction` - The `Direction` to move focus (e.g., `Direction::East`).
/// * `current_window` - The `Entity` of the currently focused `Window`.
/// * `strip` - A reference to the active `LayoutStrip`.
/// * `windows` - A query for all `Window` components.
///
/// # Returns
///
/// `Some(Entity)` with the entity of the newly focused window, otherwise `None`.
fn focus_with_verification(
    entity: Entity,
    focused_entity: Option<Entity>,
    windows: &Windows,
    apps: &Query<&Application>,
    commands: &mut Commands,
) {
    if focused_entity != Some(entity)
        && let Ok(mut entity_commands) = commands.get_entity(entity)
    {
        // What the target's app has focused now; see VerifyFocus.
        let was = windows
            .get(entity)
            .and_then(|window| windows.find_parent(window.id()))
            .and_then(|(_, _, app)| apps.get(app).ok())
            .and_then(|app| app.focused_window_id().ok());
        entity_commands.try_insert((
            crate::ecs::VerifyFocus { was },
            Timeout::for_component::<crate::ecs::VerifyFocus>(
                crate::ecs::focus::VERIFY_FOCUS_TIMEOUT,
            ),
        ));
    }
    commands.focus_entity(entity, true);
}

fn command_move_focus(
    mut messages: MessageReader<Event>,
    windows: Windows,
    workspaces: Query<(&LayoutStrip, Entity, Option<&NativeFullscreenMarker>)>,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    apps: Query<&Application>,
    mut commands: Commands,
) {
    let Some(Operation::Focus(direction)) =
        filter_window_operations(&mut messages, |op| matches!(op, Operation::Focus(_))).next()
    else {
        return;
    };

    let active_strip = active_display.active_strip();

    // On a fullscreen space, swap to the last column in the workspace.
    if let Some(NativeFullscreenMarker {
        layout_strip,
        workspace_id,
        index: _,
    }) = active_display.fullscreen()
        && matches!(direction, Direction::West)
    {
        let mut strip = workspaces
            .into_iter()
            .find_map(|(strip, entity, _)| (entity == *layout_strip).then_some(strip));
        if strip.is_none() {
            strip = workspaces
                .into_iter()
                .find_map(|(strip, _, _)| (strip.id() == *workspace_id).then_some(strip));
        }

        if let Some(entity) = strip.and_then(|strip| strip.last().ok().and_then(|col| col.top())) {
            debug!("fullscreen: swap raising {entity}");
            focus_with_verification(
                entity,
                windows.focused().map(|(_, e)| e),
                &windows,
                &apps,
                &mut commands,
            );
        }
        return;
    }

    let Some((_, focused_entity)) = windows.focused() else {
        return;
    };

    if let Some((_, _, Some(Unmanaged::Floating))) = windows.get_managed(focused_entity)
        && !matches!(direction, Direction::Nth(_))
    {
        if let Some(entity) = nearest_float_in_direction(
            direction,
            focused_entity,
            &windows,
            &window_manager,
            active_strip.id(),
            active_display.bounds(),
        ) {
            focus_with_verification(entity, Some(focused_entity), &windows, &apps, &mut commands);
        }
        return;
    }

    // If focus is on a window that no longer lives in the active strip
    // (e.g. it just became floating, was minimised on another row, or
    // the OS handed focus to a window we don't track on this strip),
    // `neighbour_in_direction` would return None and the user would
    // be unable to leave that window. Enter the active strip from the
    // appropriate side so subsequent presses behave normally.
    let candidate = if active_strip.contains(focused_entity) {
        let frame_of = |entity| windows.layout_frame(entity);
        neighbour_in_direction(direction, focused_entity, active_strip, frame_of).or_else(|| {
            // At the right edge going East, enter the fullscreen workspaces.
            // On a tiled row that edge is the screen's: nothing east of it.
            (matches!(direction, Direction::East)
                && (active_strip.is_tiled()
                    || active_strip.right_neighbour(focused_entity).is_none()))
            .then(|| {
                workspaces
                    .iter()
                    .find(|(strip, _, fullscreen)| {
                        fullscreen.is_some() && strip.id() != active_strip.id()
                    })
                    .and_then(|(strip, _, _)| strip.get(0).ok().and_then(|col| col.top()))
            })
            .flatten()
        })
    } else {
        match direction {
            Direction::East | Direction::First => {
                active_strip.first().ok().and_then(|col| col.top())
            }
            Direction::West | Direction::Last => active_strip.last().ok().and_then(|col| col.top()),
            Direction::Nth(index) => active_strip
                .get(*index)
                .ok()
                .and_then(|column| column.top()),
            Direction::North | Direction::South => None,
        }
    };

    if let Some(entity) = candidate {
        focus_with_verification(entity, Some(focused_entity), &windows, &apps, &mut commands);
        // Explicitly reshuffle so the target window is brought into view.
        // This avoids a race where focus-follows-mouse leaves skip_reshuffle
        // set, causing the WindowFocused handler to skip the reshuffle.
        commands.reshuffle_around(entity);
        return;
    }

    // Nothing further that way on this display: try the one beyond it.
    if matches!(direction, Direction::North | Direction::South)
        && active_display.other().any(|other| {
            distance_toward(active_display.bounds(), other.bounds(), direction).is_some()
        })
    {
        commands.trigger(SendMessageTrigger(Event::Command {
            command: Command::Mouse(MouseMove::ToDisplay(direction.clone())),
        }));
    }
}

fn command_focus_unmanaged(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    focus_history: Res<FocusHistory>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::FocusUnmanaged))
        .next()
        .is_none()
    {
        return;
    }

    let display_bounds = active_display.bounds();
    let workspace_id = active_display.active_strip().id();
    let visible_floats =
        visible_floating_entities(&windows, &window_manager, workspace_id, display_bounds);
    let is_visible_float = |entity: Entity| -> bool { visible_floats.contains(&entity) };

    let target = focus_history
        .last_floating(workspace_id)
        .filter(|entity| is_visible_float(*entity))
        .or_else(|| visible_floats.into_iter().next());

    if let Some(entity) = target {
        commands.focus_entity(entity, true);
    }
}

fn command_focus_managed(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    focus_history: Res<FocusHistory>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::FocusManaged))
        .next()
        .is_none()
    {
        return;
    }

    let active_strip = active_display.active_strip();
    let workspace_id = active_strip.id();

    let target = focus_history
        .last_managed(workspace_id)
        .filter(|entity| active_strip.contains(*entity))
        .or_else(|| active_strip.all_columns().into_iter().next());

    if let Some(entity) = target {
        commands.focus_entity(entity, true);
        commands.reshuffle_around(entity);
    }
}

fn command_raise_floating(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    focus_history: Res<FocusHistory>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::RaiseFloating))
        .next()
        .is_none()
    {
        return;
    }

    let display_bounds = active_display.bounds();
    let workspace_id = active_display.active_strip().id();
    let visible_floats =
        visible_floating_entities(&windows, &window_manager, workspace_id, display_bounds);
    let is_visible_float = |entity: Entity| -> bool { visible_floats.contains(&entity) };

    let target = focus_history
        .last_floating(workspace_id)
        .filter(|entity| is_visible_float(*entity))
        .or_else(|| visible_floats.first().copied());

    for (_, entity) in windows.iter() {
        if is_visible_float(entity) && Some(entity) != target {
            commands.trigger(RaiseWindow {
                entity,
                with_strip: false,
            });
        }
    }

    if let Some(entity) = target {
        commands.focus_entity(entity, true);
    }
}

/// Focus-and-raise are deliberately coupled here: macOS AX raise can't lift a
/// window above another app's frontmost window, so the target's app must be
/// made frontmost. Other windows in the new top tier are raised within their
/// own apps' stacks as a best-effort.
fn command_toggle_floating_layer(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    mut floating_layers: Query<&mut FloatingLayer>,
    focus_history: Res<FocusHistory>,
    window_manager: Res<WindowManager>,
    windows: Windows,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| {
        matches!(op, Operation::ToggleFloatingLayer)
    })
    .next()
    .is_none()
    {
        return;
    }

    let display_bounds = active_display.bounds();
    let active_strip = active_display.active_strip();
    let workspace_id = active_strip.id();

    let floating_front = floating_layers
        .iter_mut()
        .find_map(|mut layer| {
            if layer.workspace_id == workspace_id {
                layer.flip();
                Some(layer.front)
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            let layer = FloatingLayer::new(workspace_id);
            commands.spawn((layer, ChildOf(active_display.entity())));
            false
        });

    let visible_floats =
        visible_floating_entities(&windows, &window_manager, workspace_id, display_bounds);
    let visible_float = |entity: Entity| -> bool {
        visible_floats.contains(&entity) && !active_strip.contains(entity)
    };

    let target = if floating_front {
        focus_history
            .last_floating(workspace_id)
            .filter(|entity| visible_float(*entity))
            .or_else(|| visible_floats.iter().copied().find(|e| visible_float(*e)))
    } else {
        focus_history
            .last_managed(workspace_id)
            .filter(|entity| active_strip.contains(*entity))
            .or_else(|| active_strip.all_columns().into_iter().next())
    };

    if floating_front {
        windows
            .iter()
            .filter_map(|(_, e)| visible_float(e).then_some(e))
            .for_each(|entity| {
                commands.trigger(RaiseWindow {
                    entity,
                    with_strip: false,
                });
            });
    } else if let Some(entity) = target {
        commands.trigger(RaiseWindow {
            entity,
            with_strip: true,
        });
    }

    debug!("floating layer -> front: {floating_front}");
}

/// Handles the "swap" command, swapping the positions of the current window with another window in a specified direction.
///
/// # Arguments
///
/// * `direction` - The `Direction` to swap the window (e.g., `Direction::West`).
/// * `current` - The `Entity` of the currently focused `Window`.
/// * `active_display` - A mutable reference to the `ActiveDisplayMut` representing the active display.
/// * `windows` - A mutable query for all `Window` components.
/// * `commands` - Bevy commands to trigger events.
///
/// # Returns
///
/// `Some(Entity)` with the entity that was swapped with, otherwise `None`.
#[instrument(level = Level::DEBUG, skip_all)]
fn command_swap_focus(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut active_display: ActiveDisplayMut,
    mut commands: Commands,
) {
    let Some(Operation::Swap(direction)) =
        filter_window_operations(&mut messages, |op| matches!(op, Operation::Swap(_))).next()
    else {
        return;
    };

    let active_strip = active_display.active_strip();
    let mut handler = || {
        let (_, current) = windows.focused()?;
        let index = active_strip.index_of(current).ok()?;
        let frame_of = |entity| windows.layout_frame(entity);
        let other_window = neighbour_in_direction(direction, current, active_strip, frame_of)?;
        let new_index = active_strip.index_of(other_window).ok()?;
        debug!(
            "swap {direction:?}: current={current} idx={index}, other={other_window} idx={new_index}, strip_len={}",
            active_strip.len()
        );

        if index == new_index
            && let Some(Column::Stack(stack)) = active_strip.get_column_mut(index)
        {
            let pos_a = stack.iter().position(|i| i.contains(current))?;
            let pos_b = stack.iter().position(|i| i.contains(other_window))?;
            stack.swap(pos_a, pos_b);
        } else if active_strip.is_tiled() {
            // The two columns trade places, and so their tiles; nothing
            // between them moves.
            active_strip.swap(index, new_index);
        } else if index < new_index {
            (index..new_index).for_each(|idx| active_strip.swap(idx, idx + 1));
        } else {
            (new_index..index)
                .rev()
                .for_each(|idx| active_strip.swap(idx, idx + 1));
        }
        Some(current)
    };

    // Keep the focused window on-screen, but don't anchor it: if its new
    // layout slot is already visible with the strip where it is, the strip
    // stays put and per-window animation slides the window into the slot.
    // Only when the slot would fall off the edge does the strip scroll —
    // and only by the shortfall.
    let swapped = handler();
    if let Some(window) = swapped {
        commands.ensure_visible(window);
    } else {
        debug!(
            "swap {direction:?}: handler returned None (focused={:?}, strip_len={})",
            windows.focused().map(|(_, e)| e),
            active_strip.len()
        );
    }

    // Only fall through to the neighbouring display when there was nothing to
    // swap with in the first place. Re-querying the strip here would look at
    // the layout *after* the swap, where the window has by definition no
    // neighbour left in that direction - which sent every successful vertical
    // swap straight on to the other display.
    if swapped.is_none() {
        // Check if the movement can swap to another display.
        let bounds = active_display.bounds();
        let Some(other_display) = active_display.other().next() else {
            return;
        };
        let change_display = match direction {
            Direction::North => bounds.min.y > other_display.bounds().min.y,
            Direction::South => bounds.min.y < other_display.bounds().min.y,
            _ => false,
        };
        debug!("swapping window to another display: {change_display}");
        if change_display {
            commands.trigger(SendMessageTrigger(Event::Command {
                command: Command::Window(Operation::ToNextDisplay(MoveFocus::Follow)),
            }));
        }
    }
}

/// Records that the user placed this strip's offset by hand, so
/// `reshuffle_layout_strip` keeps it instead of re-deriving one from the
/// focused window's frame. The mark is dropped again as soon as the strip's
/// shape changes, the user swipes, or a window has to be scrolled into view.
fn mark_manual_offset(
    strip_entity: Entity,
    strip: &LayoutStrip,
    windows: &Windows,
    commands: &mut Commands,
) {
    if let Ok(mut entity_commands) = commands.get_entity(strip_entity) {
        entity_commands.try_insert(ManualStripOffset {
            signature: strip_signature(strip, windows),
        });
    }
}

/// Centers the focused window on the active display.
fn command_center_window(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Center))
        .next()
        .is_none()
    {
        return;
    }

    if let Some((_, entity)) = windows.focused()
        && let Some(size) = windows.size(entity)
        && let Some(mut origin) = windows.origin(entity)
    {
        let strip = active_display.active_strip();
        // A tiled row never scrolls; floating windows still centre.
        if strip.is_tiled() && strip.contains(entity) {
            return;
        }
        let center = active_display.bounds().center().x;
        origin.x = center - size.x / 2;

        if active_display.active_strip().contains(entity)
            && let Some(layout_position) = windows.layout_position(entity)
        {
            // Directly reposition the strip (bypasses hidden_ratio check), and
            // record the placement as deliberate so a later reshuffle — a
            // repeated focus event, a return from another display — does not
            // re-derive the offset and undo the centering.
            let strip_position = origin - layout_position.0;
            let strip_entity = active_display.active_strip_entity();
            commands.reposition_entity(strip_entity, strip_position);
            mark_manual_offset(
                strip_entity,
                active_display.active_strip(),
                &windows,
                &mut commands,
            );
        } else {
            commands.reposition_entity(entity, origin);
        }

        window_manager.warp_mouse(active_display.bounds().center());
    }
}

/// The preset after `current` in `direction`, skipping any within 0.05 of it
/// so float noise can't re-pick the current one. Past the end it wraps when
/// `cycle` is on, else stays at the end. `presets` is sorted ascending.
fn next_preset(presets: &[f64], current: f64, direction: ResizeDirection, cycle: bool) -> f64 {
    let first = presets.first().copied().unwrap_or(0.5);
    let last = presets.last().copied().unwrap_or(first);
    match direction {
        ResizeDirection::Grow => presets
            .iter()
            .copied()
            .find(|&r| r > current + 0.05)
            .unwrap_or(if cycle { first } else { last }),
        ResizeDirection::Shrink => presets
            .iter()
            .rev()
            .copied()
            .find(|&r| r < current - 0.05)
            .unwrap_or(if cycle { last } else { first }),
    }
}

/// Resizes the focused window based on preset column widths.
///
/// # Arguments
///
/// * `active_display` - A mutable reference to the `Display` resource.
/// * `focused_entity` - The `Entity` of the currently focused window.
/// * `windows` - A mutable query for all `Window` components.
/// * `commands` - Bevy commands to trigger events.
/// * `config` - The `Config` resource.
fn resize_window(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut active_display: ActiveDisplayMut,
    config: Res<Config>,
    mut commands: Commands,
) {
    let Some(operation) = filter_window_operations(&mut messages, |op| {
        matches!(op, Operation::Resize(_) | Operation::SetWidth(_))
    })
    .next() else {
        return;
    };

    let Some((frame, entity)) = windows
        .focused()
        .and_then(|(_, entity)| windows.frame(entity).zip(Some(entity)))
    else {
        return;
    };

    let widths = config.preset_column_widths();
    let cycle = config.window_resize_cycle();
    // A tiled row owns its windows' widths. On a master row the width keys
    // step the master width instead; nothing else applies.
    let strip = active_display.active_strip();
    if strip.is_tiled() && strip.contains(entity) {
        if let Operation::Resize(direction) = operation
            && let Tiling::Master(ratio) = *strip.tiling()
        {
            let presets = widths
                .iter()
                .copied()
                .filter(|&r| 0.05 < r && r < 0.95)
                .collect::<Vec<_>>();
            strip.set_tiling(Tiling::Master(next_preset(
                &presets, ratio, *direction, cycle,
            )));
        }
        return;
    }

    if windows.full_width(entity).is_some()
        && let Ok(mut cmds) = commands.get_entity(entity)
    {
        cmds.try_remove::<FullWidthMarker>();
    }

    let viewport = active_display.actual_bounds(&config);
    let current_ratio = f64::from(frame.width()) / f64::from(viewport.width());
    let next_ratio = match operation {
        Operation::SetWidth(ratio) if ratio.is_finite() && *ratio > 0.0 => *ratio,
        Operation::Resize(direction) => next_preset(&widths, current_ratio, *direction, cycle),
        _ => return,
    };

    let new_width = round_px(next_ratio * f64::from(viewport.width()));
    let size = Size::new(new_width, frame.height());

    // Resize all windows in the column so stacked siblings share the new width.
    let strip = active_display.active_strip();
    if let Some(Column::Stack(stack)) = strip
        .index_of(entity)
        .ok()
        .and_then(|idx| strip.get(idx).ok())
    {
        for sibling in stack.iter().flat_map(StackItem::window_iter) {
            if sibling != entity
                && let Some(size) = windows.size(sibling)
            {
                commands.resize_entity(sibling, size.with_x(new_width));
            }
        }
    }

    commands.resize_entity(entity, size);
    commands.reshuffle_around(entity);
}

/// Cycles the focused window's height through `preset_stack_heights`, letting
/// the neighbour below (or above, when the window is last) absorb the
/// difference.
///
/// Only stacks are eligible: `binpack_heights` stretches a column's lone member
/// to the full strip height, so a height written anywhere else is overwritten on
/// the next layout pass.
fn resize_window_vertical(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    let Some(&Operation::ResizeVertical(direction)) =
        filter_window_operations(&mut messages, |op| {
            matches!(op, Operation::ResizeVertical(_))
        })
        .next()
    else {
        return;
    };

    let Some((_, entity)) = windows.focused() else {
        return;
    };

    let strip = active_display.active_strip();
    let Some(Column::Stack(items)) = strip
        .index_of(entity)
        .ok()
        .and_then(|index| strip.get(index).ok())
    else {
        return;
    };
    let Some(position) = items.iter().position(|item| item.contains(entity)) else {
        return;
    };
    let neighbour = if position + 1 < items.len() {
        position + 1
    } else if position > 0 {
        position - 1
    } else {
        return;
    };

    // Pending resizes count: holding the key down must keep advancing through
    // the presets instead of re-reading a mid-animation height.
    let Some((frame, neighbour_frame)) = items[position]
        .top()
        .and_then(|entity| windows.moving_frame(entity))
        .zip(
            items[neighbour]
                .top()
                .and_then(|entity| windows.moving_frame(entity)),
        )
    else {
        return;
    };

    let pair = frame.height() + neighbour_frame.height();
    if pair < 2 * MIN_WINDOW_HEIGHT {
        return;
    }

    let viewport = active_display.actual_bounds(&config);
    let current_ratio = f64::from(frame.height()) / f64::from(viewport.height());
    let heights = config.preset_stack_heights();
    let fallback = *heights.first().unwrap_or(&0.5);
    let cycle = config.window_resize_cycle();
    // The presets are assumed sorted ascending, and the `0.05` dead-band keeps
    // float noise from re-selecting the preset the window is already at.
    let next_ratio = match direction {
        ResizeDirection::Grow => heights
            .iter()
            .copied()
            .find(|&r| r > current_ratio + 0.05)
            .unwrap_or_else(|| {
                if cycle {
                    fallback
                } else {
                    *heights.last().unwrap_or(&fallback)
                }
            }),
        ResizeDirection::Shrink => heights
            .iter()
            .rev()
            .copied()
            .find(|&r| r < current_ratio - 0.05)
            .unwrap_or_else(|| {
                if cycle {
                    *heights.last().unwrap_or(&fallback)
                } else {
                    fallback
                }
            }),
    };
    let new_height = round_px(next_ratio * f64::from(viewport.height()))
        .clamp(MIN_WINDOW_HEIGHT, pair - MIN_WINDOW_HEIGHT);

    // Leaving the pair's total untouched is what makes the new height survive
    // `binpack_heights`, which hands the column's last item the remainder.
    for (item, height) in [
        (&items[position], new_height),
        (&items[neighbour], pair - new_height),
    ] {
        for entity in item.window_iter() {
            if let Some(size) = windows.size(entity) {
                commands.resize_entity(entity, size.with_y(height));
            }
        }
    }
}

fn full_width_window(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut active_display: ActiveDisplayMut,
    config: Res<Config>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::FullWidth))
        .next()
        .is_none()
    {
        return;
    }

    let Some((_, entity)) = windows.focused() else {
        return;
    };
    // A tiled row owns its windows' sizes, and full width would unstack.
    let strip = active_display.active_strip();
    if strip.is_tiled() && strip.contains(entity) {
        return;
    }

    let viewport = active_display.actual_bounds(&config);

    if let Some(marker) = windows.full_width(entity) {
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<FullWidthMarker>();
        }
        let w = round_px(marker.width_ratio * f64::from(viewport.width()));
        let bounds = active_display.actual_bounds(&config).size().with_x(w);
        commands.resize_entity(entity, bounds);
    } else {
        let strip = active_display.active_strip();
        if strip
            .index_of(entity)
            .ok()
            .and_then(|idx| strip.get(idx).ok())
            .is_some_and(|col| matches!(col, Column::Stack(_)))
        {
            _ = strip.unstack(entity);
        }
        let width_ratio = windows.width_ratio(entity).unwrap_or(0.5);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert(FullWidthMarker { width_ratio });
        }
        commands.resize_entity(entity, Size::new(viewport.width(), viewport.height()));
        commands.reshuffle_around(entity);
    }
}

/// Toggles the managed state of the focused window.
/// If the window is currently unmanaged, it becomes managed. If managed, it becomes unmanaged (floating).
///
/// # Arguments
///
/// * `focused_entity` - The `Entity` of the currently focused window.
/// * `windows` - A mutable query for `Window` components, their `Entity`, and whether they have the `Unmanaged` marker.
/// * `commands` - Bevy commands to modify entities.
fn manage_window(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut workspaces: Query<(&mut LayoutStrip, Has<ActiveWorkspaceMarker>)>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Manage))
        .next()
        .is_none()
    {
        return;
    }

    let Some((window, entity, unmanaged)) = windows
        .focused()
        .and_then(|(_, entity)| windows.get_managed(entity))
    else {
        return;
    };
    debug!(
        "window: {} {entity} unmanaged: {}.",
        window.id(),
        unmanaged.is_some()
    );
    let was_unmanaged = unmanaged.is_some();
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        if was_unmanaged {
            entity_commands.try_remove::<Unmanaged>();
        } else {
            entity_commands.try_insert(Unmanaged::Floating);
        }
    }

    // Going floating -> managed only flips the component. Nothing else in
    // the pipeline reinserts the window into a strip, so if it had been
    // stripped of membership (the spawn-floating path in
    // window_unmanaged_trigger strip.removes) the toggle is invisible — the
    // window stays where it floated and the user thinks the keybind is
    // broken. Append to the active strip and reshuffle so the layout
    // pipeline tiles it.
    if was_unmanaged
        && !workspaces.iter().any(|(strip, _)| strip.contains(entity))
        && let Some(mut strip) = workspaces
            .iter_mut()
            .find_map(|(strip, active)| active.then_some(strip))
    {
        strip.append(entity);
        commands.reshuffle_around(entity);
    }
}

/// Closes the focused window by pressing its close button. Its removal flows
/// through the usual destroyed-window path.
fn close_window(mut messages: MessageReader<Event>, windows: Windows) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Close))
        .next()
        .is_none()
    {
        return;
    }
    let Some((window, _)) = windows.focused() else {
        debug!("no focused window to close");
        return;
    };
    _ = window
        .close()
        .inspect_err(|err| warn!("closing window {}: {err}", window.id()));
}

/// Copies a `[windows]` configuration rule for the focused window to the
/// clipboard, so the user can paste a working starting point instead of
/// guessing the app's bundle id and transcribing the title into a regex.
fn copy_window_rule(
    mut messages: MessageReader<Event>,
    windows: Windows,
    apps: Query<&Application>,
    dialect: Res<SnippetDialect>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::CopyRule))
        .next()
        .is_none()
    {
        return;
    }

    let Some((window, app)) = windows
        .focused()
        .and_then(|(window, _)| windows.find_parent(window.id()))
        .and_then(|(window, _, app_entity)| Some((window, apps.get(app_entity).ok()?)))
    else {
        debug!("no focused window to copy a rule for");
        return;
    };

    let bundle_id = app.bundle_id().unwrap_or_default();
    let title = window.title().unwrap_or_default();
    let role = window.role().unwrap_or_default();
    let subrole = window.subrole().unwrap_or_default();
    let snippet = window_rule_snippet(
        *dialect,
        &RuleSubject {
            app_name: app.name(),
            bundle_id: &bundle_id,
            title: &title,
            role: &role,
            subrole: &subrole,
        },
    );

    if crate::pasteboard::copy_to_clipboard(&snippet) {
        info!("copied window rule for {}", app.name());
        commands.flash_message("Window rule copied".to_owned(), 1.5);
    } else {
        error!("unable to copy the window rule to the clipboard");
    }
}

/// Moves the focused tiled window, with its tab group, to another display: the
/// first other display for `ToNextDisplay`, or the nearest one in a direction
/// for `ToDisplay`. Like a virtual workspace move, it lands as a new column
/// right of the target row's active column, sized for the target viewport.
/// It leaves every row it was in, including a parked one focus was left on.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[instrument(level = Level::DEBUG, skip_all)]
fn to_next_display(
    mut messages: MessageReader<Event>,
    windows: Windows,
    displays: Query<(&Display, Option<&DockPosition>, Has<ActiveDisplayMarker>)>,
    mut rows: RowsWithVisibility,
    opened_beside: Query<&OpenedBeside>,
    mut saved_focus: Query<&mut PreviousStripPosition>,
    mut focus_history: ResMut<FocusHistory>,
    window_manager: Res<WindowManager>,
    config: Res<Config>,
    time: Res<Time>,
    mut commands: Commands,
) {
    let Some((direction, move_focus)) = filter_window_operations(&mut messages, |op| {
        matches!(op, Operation::ToNextDisplay(_) | Operation::ToDisplay(_))
    })
    .find_map(|op| match op {
        Operation::ToNextDisplay(move_focus) => Some((None, *move_focus)),
        Operation::ToDisplay(direction) => Some((Some(direction.clone()), MoveFocus::Follow)),
        _ => None,
    }) else {
        return;
    };

    let Some((window, entity, unmanaged)) = windows
        .focused()
        .and_then(|(_, entity)| windows.get_managed(entity))
    else {
        return;
    };
    if unmanaged.is_some() {
        return;
    }

    let Some((source, source_dock, _)) = displays.iter().find(|(_, _, active)| *active) else {
        return;
    };
    let mut others = displays.iter().filter(|(_, _, active)| !active);
    let target = match &direction {
        None => others.next(),
        Some(direction) => others
            .filter_map(|other| {
                distance_toward(source.bounds(), other.0.bounds(), direction).zip(Some(other))
            })
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, other)| other),
    };
    let Some((target, target_dock, _)) = target else {
        debug!("no display to move window to.");
        return;
    };

    // Find the target strip before touching the source, so a missing one (a
    // native fullscreen Space, say) leaves the window where it is.
    let Ok(target_space_id) = window_manager.active_display_space(target.id()) else {
        return;
    };
    // The target: its display's selected row, not the one on screen.
    let Some(target_row) = rows
        .iter()
        .find(|(_, strip, active, selected, parented)| {
            *selected && !*active && *parented && strip.id() == target_space_id
        })
        .map(|(row, ..)| row)
    else {
        debug!("no selected strip on display {}.", target.id());
        return;
    };
    // The row that really holds the window: the one on screen, unless focus was
    // left on a window of a parked row by switching to an empty one (WSD-6).
    let Some((source_row, source_shown)) = rows
        .iter()
        .filter(|(row, strip, ..)| *row != target_row && strip.contains(entity))
        .min_by_key(|(_, _, active, ..)| !*active)
        .map(|(row, _, active, ..)| (row, active))
    else {
        debug!("window {entity} is in no row to move from.");
        return;
    };
    debug!(
        "moving window (id {}, {entity}) to display {}.",
        window.id(),
        target.id()
    );

    let moving = rows
        .get(source_row)
        .ok()
        .and_then(|(_, strip, ..)| strip.tab_group(entity))
        .unwrap_or_else(|| vec![entity]);
    // Out of any other row it was left in, so it never ends up in two. Only rows
    // holding one are touched: `remove` marks a row changed even when it is a no-op.
    for (row, mut strip, ..) in &mut rows {
        if row != source_row
            && row != target_row
            && moving.iter().any(|member| strip.contains(*member))
        {
            for member in &moving {
                strip.remove(*member);
            }
        }
    }
    let Ok([(_, mut source_strip, ..), (_, mut target_strip, ..)]) =
        rows.get_many_mut([source_row, target_row])
    else {
        return;
    };
    // The rest of its tab group first, so the focused window leaves as a lone
    // column and hands off by the rule (a native tab never hands off).
    for member in moving.iter().filter(|&&member| member != entity) {
        source_strip.remove(*member);
    }
    // Follow: the source Space now remembers the successor, too.
    let next = leave_row(
        &mut source_strip,
        entity,
        &mut focus_history,
        time.elapsed(),
        Some(entity),
        opened_beside.get(entity).ok().map(|beside| beside.0),
    );
    if let Some(next) = next {
        commands.reshuffle_around(next);
        // Stay from a parked row: the history above remembers the successor,
        // but focusing it would show that row again, and the move-detection
        // check would pull the window back into it before macOS lands the move.
        if matches!(move_focus, MoveFocus::Stay) && source_shown {
            commands.focus_entity(next, false);
        }
    }
    // A parked row focuses its saved window when shown again: make that the
    // successor, as a virtual move does, not the window that just left.
    if !source_shown && let Ok(mut parked) = saved_focus.get_mut(source_row) {
        parked.focus = next;
    }
    // Its anchor stays behind on the source row.
    if let Ok(mut entity_commands) = commands.get_entity(entity) {
        entity_commands.try_remove::<OpenedBeside>();
    }

    // niri: the window becomes a new column right of the target's active one.
    let anchor = focus_history
        .last_managed(target_space_id)
        .filter(|anchor| target_strip.contains(*anchor) && !moving.contains(anchor))
        .or_else(|| column_closest_to_center(&target_strip, target, &windows));
    let index = anchor
        .and_then(|anchor| target_strip.index_of(anchor).ok())
        .map_or(target_strip.len(), |index| index + 1);
    target_strip.insert_tab_group_at(index, &moving);

    // Keep the width ratio against the usable viewport (dock- and
    // padding-adjusted) on both displays, and take the full usable height.
    let source_width = source.actual_display_bounds(source_dock, &config).width();
    let viewport = target.actual_display_bounds(target_dock, &config);
    for moving_entity in &moving {
        let Some(size) = windows.size(*moving_entity) else {
            continue;
        };
        let width = if source_width > 0 {
            round_px(f64::from(size.x) / f64::from(source_width) * f64::from(viewport.width()))
        } else {
            size.x
        };
        commands.resize_entity(*moving_entity, Size::new(width, viewport.height()));
        commands.reposition_entity(
            *moving_entity,
            Origin::new(viewport.center().x - width / 2, viewport.min.y),
        );
    }
    commands.reshuffle_around(entity);

    if matches!(move_focus, MoveFocus::Follow) {
        window_manager.warp_mouse(target.bounds().center());
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_remove::<FocusedMarker>();
        }
        commands.focus_entity(entity, true);
        focus_history.tiled_beside(target_space_id, entity, anchor);
    }
}

/// How far `to` lies from `from` in `direction`, centre to centre, or `None`
/// when it is not that way at all.
fn distance_toward(from: IRect, to: IRect, direction: &Direction) -> Option<i32> {
    let delta = to.center() - from.center();
    let distance = match direction {
        Direction::North => -delta.y,
        Direction::South => delta.y,
        Direction::West => -delta.x,
        Direction::East => delta.x,
        Direction::First | Direction::Last | Direction::Nth(_) => return None,
    };
    (distance > 0).then_some(distance)
}

/// Moves the mouse pointer to another display and focuses the window last
/// focused there, else its most visible window: the display the pointer is not
/// on for `ToNextDisplay`, or the nearest one in a direction for `ToDisplay`.
#[instrument(level = Level::DEBUG, skip_all)]
fn mouse_to_next_display(
    mut messages: MessageReader<Event>,
    windows: Windows,
    layout_strips: Query<(&LayoutStrip, Has<SelectedVirtualMarker>)>,
    displays: Query<(&Display, Has<ActiveDisplayMarker>)>,
    focus_history: Res<FocusHistory>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    let Some(target) = messages.read().find_map(|event| match event {
        Event::Command {
            command: Command::Mouse(target),
        } => Some(target.clone()),
        _ => None,
    }) else {
        return;
    };

    let cursor_position = window_manager.cursor_position().map(origin_from);
    let other = match &target {
        MouseMove::ToNextDisplay => cursor_position.and_then(|cursor| {
            displays
                .iter()
                .map(|(display, _)| display)
                .find(|display| !display.bounds().contains(cursor))
        }),
        MouseMove::ToDisplay(direction) => {
            let toward = |from: IRect| {
                displays
                    .iter()
                    .filter_map(|(display, _)| {
                        distance_toward(from, display.bounds(), direction).zip(Some(display))
                    })
                    .min_by_key(|(distance, _)| *distance)
                    .map(|(_, display)| display)
            };
            // The active display is where focus is, but parking the pointer on
            // a display with no windows need not make macOS activate it.
            // Falling back to the display under the pointer lets the opposite
            // direction leave such a display again.
            let active = displays
                .iter()
                .find_map(|(display, active)| active.then(|| display.bounds()));
            let under_cursor = cursor_position.and_then(|cursor| {
                displays
                    .iter()
                    .map(|(display, _)| display.bounds())
                    .find(|bounds| bounds.contains(cursor))
            });
            active
                .and_then(toward)
                .or_else(|| under_cursor.and_then(toward))
        }
    };
    let Some(other) = other else {
        debug!("no other display to move mouse to.");
        return;
    };
    let Some(other_strip) = window_manager
        .active_display_space(other.id())
        .ok()
        .and_then(|id| shown_row(layout_strips.iter(), id))
    else {
        return;
    };

    let visible_width = |frame: IRect| other.bounds().intersect(frame).width();
    // The window last focused there while any of it shows, else the widest
    // visible one.
    let remembered = focus_history
        .last_managed(other_strip.id())
        .filter(|entity| other_strip.contains(*entity))
        .and_then(|entity| windows.frame(entity).zip(Some(entity)))
        .filter(|(frame, _)| visible_width(*frame) > 0);
    let Some((frame, entity)) = remembered.or_else(|| {
        other_strip
            .all_windows()
            .iter()
            .filter_map(|entity| windows.frame(*entity).zip(Some(*entity)))
            .max_by(|left, right| {
                if visible_width(left.0) < visible_width(right.0) {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            })
    }) else {
        debug!("no suitable windows on the other display to move the mouse.");
        let center = other.bounds().center();
        window_manager.warp_mouse(center);
        // Moving the pointer alone leaves the menu bar, and with it where new
        // windows open, on the old display.
        window_manager.focus_desktop(center);
        return;
    };

    let visible_frame = other.bounds().intersect(frame);
    debug!("warping mouse to {visible_frame:?}",);
    window_manager.warp_mouse(visible_frame.center());

    commands.focus_entity(entity, true);
}

/// The row of `space` its display shows: the selected one, else the first.
/// A space has a row per virtual workspace, the dynamic spare included, so
/// any row with the right id may be an empty one the user isn't looking at.
fn shown_row<'a>(
    rows: impl Iterator<Item = (&'a LayoutStrip, bool)>,
    space: WorkspaceId,
) -> Option<&'a LayoutStrip> {
    rows.filter(|(strip, _)| strip.id() == space)
        .min_by_key(|(strip, selected)| (!*selected, strip.virtual_index))
        .map(|(strip, _)| strip)
}

/// Distributes heights equally among all windows in the currently focused stack.
fn equalize_column(
    mut messages: MessageReader<Event>,
    current_focus: Single<(&Window, Entity), With<FocusedMarker>>,
    windows: Windows,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Equalize))
        .next()
        .is_none()
    {
        return;
    }

    let (_, entity) = *current_focus;
    let active_strip = active_display.active_strip();
    let Ok(column) = active_strip
        .index_of(entity)
        .and_then(|index| active_strip.get(index))
    else {
        return;
    };

    if let Column::Stack(stack) = column {
        #[allow(clippy::cast_precision_loss)]
        let equal_height =
            active_display.actual_bounds(&config).height() / i32::try_from(stack.len()).unwrap();

        for item in &stack {
            for entity in item.window_iter() {
                if let Some(size) = windows.size(entity) {
                    commands.resize_entity(entity, size.with_y(equal_height));
                }
            }
        }
    }
}

/// Makes all columns in the active strip the same width as the focused window.
fn balance_strip(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Balance))
        .next()
        .is_none()
    {
        return;
    }
    // A tiled row owns its windows' widths.
    if active_display.active_strip().is_tiled() {
        return;
    }

    let Some((_, focused_entity)) = windows.focused() else {
        return;
    };
    let Some(focused_width) = windows.size(focused_entity).map(|s| s.x) else {
        return;
    };

    let strip = active_display.active_strip();

    for column in strip.columns() {
        if matches!(column, Column::Fullscren(_)) {
            continue;
        }

        for entity in column.window_iter() {
            if windows.full_width(entity).is_some()
                && let Ok(mut cmds) = commands.get_entity(entity)
            {
                cmds.try_remove::<FullWidthMarker>();
            }

            if let Some(size) = windows.size(entity) {
                commands.resize_entity(entity, size.with_x(focused_width));
            }
        }
    }

    commands.reshuffle_around(focused_entity);
}

/// Slides the strip so the focused window is fully visible, snapping to the
/// nearest edge: left-aligned when the window overflows left, right-aligned
/// when it overflows right. No resize — the window keeps its current size.
/// Bypasses the lazy-viewport check since the user explicitly asked to reveal.
fn snap_window(
    mut messages: MessageReader<Event>,
    windows: Windows,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::Snap))
        .next()
        .is_none()
    {
        return;
    }
    // A tiled row never scrolls.
    if active_display.active_strip().is_tiled() {
        return;
    }

    let Some((_, entity)) = windows.focused() else {
        return;
    };
    let Some(layout_position) = windows.layout_position(entity) else {
        return;
    };
    let Some(mut frame) = windows.moving_frame(entity) else {
        return;
    };

    let display_bounds = active_display.actual_bounds(&config);

    // Clamp the frame into the display and reposition the *strip* (not the
    // window) so the layout stays consistent.
    let size = frame.size();
    frame.min = clamp_origin_to_viewport(frame.min, size, display_bounds);
    frame.max = frame.min + size;

    let strip_position = frame.min - layout_position.0;
    let strip_entity = active_display.active_strip_entity();
    commands.reposition_entity(strip_entity, strip_position);
    mark_manual_offset(
        strip_entity,
        active_display.active_strip(),
        &windows,
        &mut commands,
    );
}

#[instrument(level = Level::DEBUG, skip_all)]
pub fn stack_windows_handler(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut active_display: ActiveDisplayMut,
    // config: Res<Config>,
    mut commands: Commands,
) {
    let Some(Operation::Stack(stack)) =
        filter_window_operations(&mut messages, |op| matches!(op, Operation::Stack(_))).next()
    else {
        return;
    };

    if let Some((_, entity, unmanaged)) = windows
        .focused()
        .and_then(|(_, entity)| windows.get_managed(entity))
        && unmanaged.is_none()
    {
        if windows.full_width(entity).is_some()
            && let Ok(mut entity_commands) = commands.get_entity(entity)
        {
            entity_commands.try_remove::<FullWidthMarker>();
        }
        let strip = active_display.active_strip();
        if *stack {
            _ = strip.stack(entity);
        } else {
            _ = strip.unstack(entity);
        }

        // Stacking/unstacking moves the focused window to a new column slot
        // (onto the left master, or out to its own column on the right).
        // Reshuffle around it so it is brought fully back into view; the
        // edge-clamp in reshuffle_layout_strip keeps the strip pinned so the
        // leftmost window touches the left edge and the rightmost the right.
        commands.reshuffle_around(entity);
    }
}

/// Toggles the focused window's stack between a normal split display and a
/// tabbed display (one window visible at a time, sharing the full column
/// rect — cycle with the existing `Focus` North/South directions). A no-op
/// on anything that isn't a multi-window `Column::Stack`.
#[instrument(level = Level::DEBUG, skip_all)]
pub fn toggle_tabbed_display_handler(
    mut messages: MessageReader<Event>,
    windows: Windows,
    mut active_display: ActiveDisplayMut,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| {
        matches!(op, Operation::ToggleTabbedDisplay)
    })
    .next()
    .is_none()
    {
        return;
    }

    if let Some((_, entity, unmanaged)) = windows
        .focused()
        .and_then(|(_, entity)| windows.get_managed(entity))
        && unmanaged.is_none()
    {
        let strip = active_display.active_strip();
        // `None` means the toggle wasn't applicable (not in a multi-window
        // Stack) — stay silent rather than reporting it the same way as a
        // real "turned tabs off", which previously made repeated presses on
        // a non-stacked window look like a stuck toggle.
        if let Some(now_tabbed) = strip.toggle_tabbed_display(entity) {
            commands.reshuffle_around(entity);
            commands.flash_message(
                if now_tabbed { "Tabs on" } else { "Tabs off" }.to_string(),
                1.0,
            );
        }
    }
}

/// Cycles the row on screen through scrolling → dwindle → master → scrolling.
/// Native fullscreen rows always stay scrolling.
#[instrument(level = Level::DEBUG, skip_all)]
pub fn toggle_layout(
    mut messages: MessageReader<Event>,
    mut active: Single<(Entity, &mut LayoutStrip), With<ActiveWorkspaceMarker>>,
    display: Single<(&Display, Option<&DockPosition>), With<ActiveDisplayMarker>>,
    windows: Windows,
    config: Res<Config>,
    mut commands: Commands,
) {
    if filter_window_operations(&mut messages, |op| matches!(op, Operation::ToggleLayout))
        .next()
        .is_none()
    {
        return;
    }
    let (strip_entity, strip) = &mut *active;
    if strip.is_fullscreen() {
        return;
    }
    let next = strip.tiling().next();
    match next {
        // A tiled row never scrolls: pin it to the viewport and drop any
        // deliberate offset (center, snap) left from scrolling.
        Tiling::Dwindle(_) => {
            if let Ok(mut entity_commands) = commands.get_entity(*strip_entity) {
                entity_commands.try_remove::<ManualStripOffset>();
            }
            let (display, dock) = *display;
            commands.reposition_entity(
                *strip_entity,
                display.actual_display_bounds(dock, &config).min,
            );
        }
        // Back to scrolling: scroll the strip to the focused window again.
        Tiling::Scrolling => {
            let around = windows
                .focused()
                .map(|(_, entity)| entity)
                .filter(|entity| strip.contains(*entity))
                .or_else(|| strip.columns().next().and_then(Column::top));
            if let Some(entity) = around {
                commands.reshuffle_around(entity);
            }
        }
        Tiling::Master(_) => {}
    }
    strip.set_tiling(next);
}

/// Dispatches a command based on the `CommandTrigger` event.
/// This function is a Bevy system that reacts to `CommandTrigger` events and executes the corresponding window manager command.
///
/// # Arguments
///
/// * `trigger` - The `On<CommandTrigger>` event trigger containing the command to process.
/// * `windows` - A query for `Window` components, their `Entity`, and whether they have the `Unmanaged` marker.
/// * `active_display` - A mutable reference to the `ActiveDisplayMut` resource.
/// * `window_manager` - The `WindowManager` resource for interacting with the window management logic.
/// * `commands` - Bevy commands to trigger events and modify entities.
/// * `config` - The `Config` resource, containing application settings.
#[instrument(level = Level::DEBUG, skip_all)]
pub fn command_quit_handler(
    mut messages: MessageReader<Event>,
    window_manager: Res<WindowManager>,
) {
    if messages.read().any(|event| {
        matches!(
            event,
            Event::Command {
                command: Command::Quit
            }
        )
    }) {
        _ = window_manager.quit();
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
pub fn command_restart_handler(mut messages: MessageReader<Event>) {
    if messages.read().any(|event| {
        matches!(
            event,
            Event::Command {
                command: Command::Restart
            }
        )
    }) && let Err(err) = crate::platform::service::Service::request_restart()
    {
        error!("failed to restart service: {err}");
    }
}

#[instrument(level = Level::DEBUG, skip_all)]
fn print_internal_state_handler(
    mut messages: MessageReader<Event>,
    focused: Query<(&Window, Entity), With<FocusedMarker>>,
    windows: Query<(&Window, Entity, &ChildOf, Option<&Unmanaged>)>,
    apps: Query<&Application>,
    workspaces: StripsWithVisibility,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
) {
    if !messages.read().any(|event| {
        matches!(
            event,
            Event::Command {
                command: Command::PrintState,
            }
        )
    }) {
        return;
    }

    let focused = focused.single().ok();
    let print_window = |(window, entity, child, unmanaged): (
        &Window,
        Entity,
        &ChildOf,
        Option<_>,
    )| {
        let bundle_id = apps
            .get(child.parent())
            .ok()
            .and_then(|app| app.bundle_id())
            .unwrap_or_default();
        format!(
            "\tid: {}, {entity}, {}:{}, {}x{}{}{}, bundle: {}, role: {}, subrole: {}, title: '{:.70}'",
            window.id(),
            window.frame().min.x,
            window.frame().min.y,
            window.frame().width(),
            window.frame().height(),
            if focused.is_some_and(|(_, focus)| focus == entity) {
                ", focused"
            } else {
                ""
            },
            unmanaged.map(|m| format!(", {m:?}")).unwrap_or_default(),
            bundle_id,
            window.role().unwrap_or_default(),
            window.subrole().unwrap_or_default(),
            window.title().unwrap_or_default()
        )
    };

    let mut seen = EntityHashSet::new();

    for (display, display_entity, active) in displays {
        for (_, strip, strip_entity, active_workspace, selected) in workspaces
            .iter()
            .filter(|child| child.0.parent() == display_entity)
        {
            let windows = strip
                .all_windows()
                .iter()
                .filter_map(|entity| windows.get(*entity).ok())
                .inspect(|(_, entity, _, _)| {
                    seen.insert(*entity);
                })
                .map(print_window)
                .collect::<Vec<_>>();

            let display_id = display.id();
            info!(
                "Display {display_id}{}, workspace id {} ({strip_entity}){}{}: {strip}:\n{}",
                if active { ", active" } else { "" },
                strip.id(),
                if active_workspace { ", active" } else { "" },
                if selected { ", selected" } else { "" },
                windows.join("\n")
            );
        }
    }

    let remaining = windows
        .iter()
        .filter(|entity| !seen.contains(&entity.1))
        .map(print_window)
        .collect::<Vec<_>>();
    info!("Remaining:\n{}", remaining.join("\n"));

    if let Some(pool) = bevy::tasks::ComputeTaskPool::try_get() {
        info!("Running with {} threads", pool.thread_num());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::prelude::*;

    #[test]
    fn shown_row_is_the_selected_row_not_the_spare() {
        let (row0, row1, spare, other) = (
            LayoutStrip::new(7, 0),
            LayoutStrip::new(7, 1),
            LayoutStrip::new(7, 2),
            LayoutStrip::new(8, 0),
        );
        let index = |rows: Vec<(&LayoutStrip, bool)>| {
            shown_row(rows.into_iter(), 7).map(|strip| strip.virtual_index)
        };
        // The spare comes first, as it did live.
        assert_eq!(
            index(vec![
                (&spare, false),
                (&other, true),
                (&row1, true),
                (&row0, false)
            ]),
            Some(1)
        );
        assert_eq!(index(vec![(&spare, false), (&row0, false)]), Some(0));
        assert_eq!(index(vec![(&other, true)]), None);
    }

    fn setup_world_with_layout() -> (World, LayoutStrip, Vec<Entity>) {
        let mut world = World::new();
        // e0, e1 are stacked, e2 is single, e3 is single
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]); // This will become a stack
        strip.append(entities[1]);
        strip.append(entities[2]);
        strip.append(entities[3]);
        strip.stack(entities[1]).unwrap(); // Stack e1 onto e0

        (world, strip, entities)
    }

    #[test]
    fn test_get_window_in_direction_simple() {
        let (_world, strip, entities) = setup_world_with_layout();
        let e0 = entities[0];
        let e2 = entities[2];
        let e3 = entities[3];
        let east = Direction::East;
        let west = Direction::West;

        // From e2, east should be e3, west should be e0 (top of stack)
        assert_eq!(get_window_in_direction(&east, e2, &strip), Some(e3));
        assert_eq!(get_window_in_direction(&west, e2, &strip), Some(e0));

        // From e3, west is e2, east is None
        assert_eq!(get_window_in_direction(&west, e3, &strip), Some(e2));
        assert_eq!(get_window_in_direction(&east, e3, &strip), None);

        // From e0, east is e2, west is None
        assert_eq!(get_window_in_direction(&east, e0, &strip), Some(e2));
        assert_eq!(get_window_in_direction(&west, e0, &strip), None);
    }

    #[test]
    fn test_get_window_in_direction_stacked() {
        let (_world, strip, entities) = setup_world_with_layout();
        let e0 = entities[0];
        let e1 = entities[1];
        let north = Direction::North;
        let south = Direction::South;

        // From e0 (top of stack), south should be e1, north is None
        assert_eq!(get_window_in_direction(&south, e0, &strip), Some(e1));
        assert_eq!(get_window_in_direction(&north, e0, &strip), None);

        // From e1 (bottom of stack), north should be e0, south is None
        assert_eq!(get_window_in_direction(&north, e1, &strip), Some(e0));
        assert_eq!(get_window_in_direction(&south, e1, &strip), None);
    }

    #[test]
    fn test_get_window_in_direction_adjacent_stacks() {
        // Layout: [Stack(e0, e1), Stack(e2, e3)]
        let mut world = World::new();
        let entities = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();

        let mut strip = LayoutStrip::default();
        strip.append(entities[0]);
        strip.append(entities[1]);
        strip.append(entities[2]);
        strip.append(entities[3]);
        strip.stack(entities[1]).unwrap(); // Stack e1 onto e0: [Stack(e0, e1), e2, e3]
        strip.stack(entities[3]).unwrap(); // Stack e3 onto e2: [Stack(e0, e1), Stack(e2, e3)]

        let east = Direction::East;
        let west = Direction::West;

        // From e0 (top-left), east should go to e2 (top-right)
        assert_eq!(
            get_window_in_direction(&east, entities[0], &strip),
            Some(entities[2])
        );
        // From e1 (bottom-left), east should go to e3 (bottom-right)
        assert_eq!(
            get_window_in_direction(&east, entities[1], &strip),
            Some(entities[3])
        );
        // From e2 (top-right), west should go to e0 (top-left)
        assert_eq!(
            get_window_in_direction(&west, entities[2], &strip),
            Some(entities[0])
        );
        // From e3 (bottom-right), west should go to e1 (bottom-left)
        assert_eq!(
            get_window_in_direction(&west, entities[3], &strip),
            Some(entities[1])
        );
    }

    #[test]
    fn pick_nearest_in_direction_east_picks_closer() {
        let mut world = World::new();
        let near = world.spawn(()).id();
        let far = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![
            (near, bevy::math::IVec2::new(10, 0)),
            (far, bevy::math::IVec2::new(50, 0)),
        ];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            Some(near),
        );
    }

    #[test]
    fn pick_nearest_in_direction_respects_cone() {
        let mut world = World::new();
        let candidate = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        // y/x ratio > 1 → outside the 45° east cone.
        let candidates = vec![(candidate, bevy::math::IVec2::new(10, 20))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            None,
        );
    }

    #[test]
    fn pick_nearest_in_direction_ignores_wrong_side() {
        let mut world = World::new();
        let west_one = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![(west_one, bevy::math::IVec2::new(-10, 0))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::East, focused, candidates),
            None,
        );
    }

    #[test]
    fn pick_nearest_in_direction_first_last_return_none() {
        let mut world = World::new();
        let any = world.spawn(()).id();
        let focused = bevy::math::IVec2::new(0, 0);
        let candidates = vec![(any, bevy::math::IVec2::new(10, 0))];
        assert_eq!(
            pick_nearest_in_direction(&Direction::First, focused, candidates.clone()),
            None,
        );
        assert_eq!(
            pick_nearest_in_direction(&Direction::Last, focused, candidates),
            None,
        );
    }

    /// A master row: e0 left, e1 above e2 on the right. Frames are
    /// strip-relative, as `frame_of` gets them from the layout.
    #[test]
    fn neighbour_in_direction_follows_master_tiles() {
        let mut world = World::new();
        let e = world.spawn_batch(vec![(), (), ()]).collect::<Vec<Entity>>();
        let mut strip = LayoutStrip::default();
        for &entity in &e {
            strip.append(entity);
        }
        strip.set_tiling(Tiling::Master(0.5));
        let frames = [
            (e[0], IRect::new(0, 0, 512, 748)),
            (e[1], IRect::new(512, 0, 1024, 374)),
            (e[2], IRect::new(512, 374, 1024, 748)),
        ];
        let frame_of = |entity: Entity| {
            frames
                .iter()
                .find(|(id, _)| *id == entity)
                .map(|&(_, frame)| frame)
        };
        let go =
            |direction: Direction, from| neighbour_in_direction(&direction, from, &strip, frame_of);

        // e1 and e2 are equally near the master: the earlier column wins.
        assert_eq!(go(Direction::East, e[0]), Some(e[1]));
        assert_eq!(go(Direction::South, e[1]), Some(e[2]));
        assert_eq!(go(Direction::North, e[2]), Some(e[1]));
        // Column order would give e1 and e2 here.
        assert_eq!(go(Direction::West, e[2]), Some(e[0]));
        assert_eq!(go(Direction::East, e[1]), None);
        // First / Last / Nth stay in column order.
        assert_eq!(go(Direction::Last, e[0]), Some(e[2]));

        // A scrolling row defers to column order, whatever the frames say.
        strip.set_tiling(Tiling::Scrolling);
        assert_eq!(
            neighbour_in_direction(&Direction::East, e[1], &strip, frame_of),
            Some(e[2])
        );
        assert_eq!(
            neighbour_in_direction(&Direction::South, e[1], &strip, frame_of),
            None
        );
    }

    /// A master row with a tabbed stack: e1 and e2 share the top-right tile,
    /// e3 sits below it.
    #[test]
    fn neighbour_in_direction_walks_a_stacked_tile_first() {
        let mut world = World::new();
        let e = world
            .spawn_batch(vec![(), (), (), ()])
            .collect::<Vec<Entity>>();
        let mut strip = LayoutStrip::default();
        for &entity in &e {
            strip.append(entity);
        }
        strip.stack(e[2]).unwrap(); // [e0, Stack(e1, e2), e3]
        strip.set_tiling(Tiling::Master(0.5));
        let tile = IRect::new(512, 0, 1024, 374);
        let frames = [
            (e[0], IRect::new(0, 0, 512, 748)),
            (e[1], tile),
            (e[2], tile),
            (e[3], IRect::new(512, 374, 1024, 748)),
        ];
        let frame_of = |entity: Entity| {
            frames
                .iter()
                .find(|(id, _)| *id == entity)
                .map(|&(_, frame)| frame)
        };
        let go =
            |direction: Direction, from| neighbour_in_direction(&direction, from, &strip, frame_of);

        // The screen alone would pick e3; the stack comes first.
        assert_eq!(go(Direction::South, e[1]), Some(e[2]));
        // At the end of the stack, the screen takes over.
        assert_eq!(go(Direction::South, e[2]), Some(e[3]));
        // Into the stack: every item is a face, and the first of equals wins.
        assert_eq!(go(Direction::North, e[3]), Some(e[1]));
    }
}
