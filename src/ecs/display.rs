use bevy::app::{App, Plugin, PreUpdate};
use bevy::ecs::change_detection::DetectChangesMut;
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::hierarchy::ChildOf;
use bevy::ecs::lifecycle::Add;
use bevy::ecs::message::MessageReader;
use bevy::ecs::observer::On;
use bevy::ecs::query::{Has, With};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, NonSend, Query, Res};
use bevy::ecs::world::World;
use bevy::math::IRect;
use bevy::platform::collections::HashSet;
use bevy::time::{Time, Timer, TimerMode};
use objc2_app_kit::NSScreen;
use objc2_core_graphics::CGDirectDisplayID;
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;
use tracing::{Level, debug, error, instrument, warn};

use crate::config::Config;
use crate::ecs::layout::{LayoutStrip, PARKED_STRIP_SLIVER};
use crate::ecs::workspace::{PreviousStripPosition, moved_from_another_row};
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, EnsureVisibleMarker, FocusedMarker, LaunchedInto,
    LayoutPosition, ManualStripOffset, NativeFullscreenMarker, Position, ReadDisplayProperties,
    RepositionMarker, SelectedVirtualMarker, SendMessageTrigger, SpawnCommandsExt, Timeout,
    Unmanaged, VerifyWindowSize,
};
use crate::events::Event;
use crate::manager::{Display, Origin, Window, WindowManager, irect_from};
use crate::platform::{PlatformCallbacks, WinID, WorkspaceId};
use crate::util::{read_screen_property, round_px};
use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::resource::Resource;
use bevy::ecs::system::ResMut;

const ORPHANED_SPACES_TIMEOUT_SEC: u64 = 30;
const DISPLAY_SETTLE_DELAY: Duration = Duration::from_millis(350);
const DISPLAY_VERIFY_DELAY: Duration = Duration::from_secs(1);
const DISPLAY_RETRY_DELAY: Duration = Duration::from_secs(1);
const DISPLAY_RETRIES: u8 = 3;

#[derive(Default, Resource)]
pub(crate) struct DisplayReconcileState {
    pending: Option<Timer>,
    retries_left: u8,
    verify_again: bool,
}

impl DisplayReconcileState {
    pub(crate) fn is_settling(&self) -> bool {
        self.pending.is_some() && self.verify_again
    }

    /// A reconcile is scheduled (settling, retrying or verifying).
    pub(crate) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

pub struct DisplayEventsPlugin;

impl Plugin for DisplayEventsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DisplayReconcileState>()
            .add_systems(
                PreUpdate,
                (display_change_handler, reconcile_displays)
                    .chain()
                    .in_set(super::PreUpdateSet::Dispatch),
            )
            .add_observer(read_display_properties_trigger)
            .add_observer(cleanup_active_display_marker);
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn cleanup_active_display_marker(
    trigger: On<Add, ActiveDisplayMarker>,
    displays: Query<(Entity, Has<ActiveDisplayMarker>), With<Display>>,
    mut commands: Commands,
) {
    for (entity, active) in displays {
        if active
            && entity != trigger.entity
            && let Ok(mut cmd) = commands.get_entity(entity)
        {
            debug!("Display id {entity} lost active marker.");
            cmd.try_remove::<ActiveDisplayMarker>();
        }
    }
}

/// Handles display change events.
#[instrument(level = Level::DEBUG, skip_all, fields(trigger))]
fn display_change_handler(
    mut messages: MessageReader<Event>,
    displays: Query<(&Display, Entity, Has<ActiveDisplayMarker>)>,
    windows: Query<Entity, With<Window>>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    if !messages
        .read()
        .any(|event| matches!(event, Event::DisplayChanged))
    {
        return;
    }

    for window in windows {
        if let Ok(mut cmd) = commands.get_entity(window) {
            cmd.try_insert(VerifyWindowSize);
        }
    }

    let Ok(active_id) = window_manager.active_display_id() else {
        error!("Unable to get active display id!");
        return;
    };

    for (display, entity, focused) in displays {
        let display_id = display.id();
        if !focused
            && display_id == active_id
            && let Ok(mut cmd) = commands.get_entity(entity)
        {
            debug!("Display id {display_id} is active");
            cmd.try_insert(ActiveDisplayMarker);
        }
    }
    commands.trigger(SendMessageTrigger(Event::SpaceChanged));
}

/// Full reconciliation of the ECS display set against the OS truth.
///
/// Runs after a brief quiet period following wake or a display notification.
/// macOS may report an empty or incomplete display list during reconfiguration;
/// applying that transient list would orphan every workspace. A successful
/// scan refreshes both display ownership and the positions of its strips.
#[allow(clippy::too_many_lines)]
pub(crate) fn reconcile_displays(
    mut messages: MessageReader<Event>,
    workspaces: Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    mut displays: Query<(&mut Display, Entity)>,
    window_manager: Res<WindowManager>,
    time: Res<Time>,
    mut state: ResMut<DisplayReconcileState>,
    mut commands: Commands,
) {
    let needs_reconcile = messages.read().fold(false, |changed, event| {
        changed
            | matches!(
                event,
                Event::SystemWoke { .. }
                    | Event::DisplayAdded { .. }
                    | Event::DisplayRemoved { .. }
                    | Event::DisplayMoved { .. }
                    | Event::DisplayResized { .. }
                    | Event::DisplayConfigured { .. }
            )
    });
    if needs_reconcile {
        state.pending = Some(Timer::new(DISPLAY_SETTLE_DELAY, TimerMode::Once));
        state.retries_left = DISPLAY_RETRIES;
        state.verify_again = true;
        return;
    }

    let Some(timer) = state.pending.as_mut() else {
        return;
    };
    timer.tick(time.delta());
    if !timer.is_finished() {
        return;
    }
    state.pending = None;

    debug!("Reconciling displays against OS after wake / resize / configure");

    let mut present_displays: HashMap<CGDirectDisplayID, _> = window_manager
        .0
        .present_displays()
        .into_iter()
        .map(|(display, workspaces)| (display.id(), (display, workspaces)))
        .collect();
    if present_displays.is_empty()
        || present_displays
            .values()
            .any(|(_, spaces)| spaces.is_empty())
    {
        if state.retries_left > 0 {
            state.retries_left -= 1;
            warn!("Display list is incomplete; retrying in {DISPLAY_RETRY_DELAY:?}");
            state.pending = Some(Timer::new(DISPLAY_RETRY_DELAY, TimerMode::Once));
            return;
        }
        warn!("Display list is still incomplete after retries; applying the latest scan");
    }

    let previous_bounds: HashMap<Entity, IRect> = displays
        .iter()
        .map(|(display, entity)| (entity, display.bounds()))
        .collect();
    let previous_parents: HashMap<Entity, IRect> = workspaces
        .iter()
        .filter_map(|(_, entity, child)| {
            let bounds = previous_bounds.get(&child?.parent())?;
            Some((entity, *bounds))
        })
        .collect();

    let existing_displays: HashMap<CGDirectDisplayID, _> = displays
        .iter()
        .map(|(display, workspaces)| (display.id(), (display, workspaces)))
        .collect();

    let present_ids = present_displays.keys().copied().collect::<HashSet<_>>();
    let existing_ids = existing_displays.keys().copied().collect::<HashSet<_>>();

    // Displays that vanished while we were away (e.g. unplugged during sleep).
    for display_id in existing_ids.difference(&present_ids) {
        let Some((display, _)) = existing_displays.get(display_id) else {
            error!("Unable to find removed display: {display_id}");
            continue;
        };
        remove_display(display, &workspaces, &displays, &mut commands);
    }

    // Displays that appeared while we were away.
    for display_id in present_ids.difference(&existing_ids) {
        let Some((display, workspace_ids)) = present_displays.remove(display_id) else {
            error!("Unable to find added display: {display_id}");
            continue;
        };
        add_display(
            display,
            &workspace_ids,
            &workspaces,
            &window_manager,
            &mut commands,
        );
    }

    // Displays that are still present: refresh their bounds (resolution or
    // menubar may have changed) and re-home any workspaces that drifted.
    // A display whose Space list couldn't be read arrives with an empty list
    // (see `present_displays`). Re-parenting then touches nothing, so it keeps
    // its rows; never make this loop drop rows missing from `workspace_ids`.
    for display_id in present_ids.intersection(&existing_ids) {
        let Some((display, workspace_ids)) = present_displays.remove(display_id) else {
            continue;
        };
        move_display(
            display,
            &workspace_ids,
            &mut displays,
            &workspaces,
            &window_manager,
            &mut commands,
        );
    }

    commands.queue(move |world: &mut World| {
        migrate_moved_windows_across_displays(world);
        refresh_display_layout(world, &previous_parents);
    });
    commands.trigger(SendMessageTrigger(Event::DisplayChanged));
    if state.verify_again {
        // A begin-configuration callback can precede the final macOS space
        // assignment even after a quiet period. Verify once more without
        // requiring a second notification from Core Graphics.
        state.verify_again = false;
        state.pending = Some(Timer::new(DISPLAY_VERIFY_DELAY, TimerMode::Once));
    }
}

/// Reconciles virtual workspaces when displays are added or removed:
/// Windows that macOS moved to a display's space are grouped by their
/// previous virtual workspace strip and placed together into a strip on
/// that display (populating its empty first workspace, or appending new
/// virtual workspaces if multiple source workspaces moved or the target
/// display already has windows).
pub(crate) fn migrate_moved_windows_across_displays(world: &mut World) {
    let mut seen = HashSet::new();
    let targets: Vec<(Entity, WorkspaceId)> = world
        .query::<(&LayoutStrip, &ChildOf, Has<NativeFullscreenMarker>)>()
        .iter(world)
        .filter_map(|(strip, child, fullscreen)| {
            (!fullscreen && seen.insert((child.parent(), strip.id())))
                .then_some((child.parent(), strip.id()))
        })
        .collect();

    let mut affected_source_spaces = HashSet::new();
    for (display_entity, target_space_id) in targets {
        migrate_moved_windows_to_space(
            world,
            display_entity,
            target_space_id,
            &mut affected_source_spaces,
        );
    }

    for source_space_id in affected_source_spaces {
        compact_workspace_strips(world, source_space_id);
    }
}

pub(crate) fn migrate_moved_windows_for_space(
    world: &mut World,
    display_entity: Entity,
    target_space_id: WorkspaceId,
) {
    let mut affected_source_spaces = HashSet::new();
    migrate_moved_windows_to_space(
        world,
        display_entity,
        target_space_id,
        &mut affected_source_spaces,
    );
    for source_space_id in affected_source_spaces {
        compact_workspace_strips(world, source_space_id);
    }
}

fn migrate_moved_windows_to_space(
    world: &mut World,
    display_entity: Entity,
    target_space_id: WorkspaceId,
    affected_source_spaces: &mut HashSet<WorkspaceId>,
) {
    let Some(display_bounds) = world.get::<Display>(display_entity).map(Display::bounds) else {
        return;
    };
    let Ok(win_ids) = world
        .resource::<WindowManager>()
        .windows_in_workspace(target_space_id)
    else {
        return;
    };

    let managed_windows: HashMap<WinID, Entity> = world
        .query::<(&Window, Entity, Has<Unmanaged>)>()
        .iter(world)
        .filter_map(|(window, entity, unmanaged)| (!unmanaged).then_some((window.id(), entity)))
        .collect();
    if managed_windows.is_empty() {
        return;
    }

    // Listed in this Space, in none of its rows, and moved from another row:
    // the same rule as detect_moved_windows' single path.
    let moved_entities: EntityHashSet = {
        let now = world.resource::<Time>().elapsed();
        let mut strip_query = world.query::<(&LayoutStrip, Has<NativeFullscreenMarker>)>();
        let strips: Vec<(&LayoutStrip, bool)> = strip_query.iter(world).collect();
        win_ids
            .into_iter()
            .filter_map(|id| managed_windows.get(&id).copied())
            .filter(|&entity| {
                !strips
                    .iter()
                    .any(|(strip, _)| strip.id() == target_space_id && strip.contains(entity))
                    && moved_from_another_row(
                        entity,
                        strips.iter().copied(),
                        world.get::<LaunchedInto>(entity),
                        now,
                    )
            })
            .collect()
    };
    if moved_entities.is_empty() {
        return;
    }

    let mut source_groups: Vec<(Entity, WorkspaceId, u32, EntityHashSet)> = world
        .query::<(Entity, &LayoutStrip, Has<NativeFullscreenMarker>)>()
        .iter(world)
        .filter_map(|(src_entity, src_strip, fullscreen)| {
            if fullscreen || src_strip.id() == target_space_id {
                return None;
            }
            let matching: EntityHashSet = src_strip
                .all_windows()
                .into_iter()
                .filter(|entity| moved_entities.contains(entity))
                .flat_map(|entity| src_strip.tab_group(entity).unwrap_or_else(|| vec![entity]))
                .collect();
            (!matching.is_empty()).then_some((
                src_entity,
                src_strip.id(),
                src_strip.virtual_index,
                matching,
            ))
        })
        .collect();
    source_groups.sort_by_key(|(_, space_id, v_idx, _)| (*space_id, *v_idx));

    if !source_groups.is_empty() {
        compact_workspace_strips(world, target_space_id);
    }

    for (src_entity, src_space_id, _, moved_in_src) in source_groups {
        affected_source_spaces.insert(src_space_id);
        let mut extracted = LayoutStrip::new(target_space_id, 0);
        if let Some(mut src_strip) = world.get_mut::<LayoutStrip>(src_entity) {
            src_strip.extract_windows_into(&moved_in_src, &mut extracted);
        }
        if extracted.len() > 0 {
            place_extracted_strip(
                world,
                display_entity,
                display_bounds,
                target_space_id,
                extracted,
            );
        }
    }
}

fn place_extracted_strip(
    world: &mut World,
    display_entity: Entity,
    display_bounds: IRect,
    target_space_id: WorkspaceId,
    mut extracted: LayoutStrip,
) {
    let empty_row_0 = world
        .query::<(Entity, &LayoutStrip, Has<NativeFullscreenMarker>)>()
        .iter(world)
        .find_map(|(entity, strip, fullscreen)| {
            (!fullscreen
                && strip.id() == target_space_id
                && strip.virtual_index == 0
                && strip.len() == 0)
                .then_some(entity)
        });

    if let Some(dst_entity) = empty_row_0 {
        if let Some(mut dst_strip) = world.get_mut::<LayoutStrip>(dst_entity) {
            dst_strip.append_strip(&mut extracted);
        }
        let any_selected = world
            .query::<(&LayoutStrip, Has<SelectedVirtualMarker>)>()
            .iter(world)
            .any(|(strip, selected)| strip.id() == target_space_id && selected);
        if !any_selected {
            world
                .entity_mut(dst_entity)
                .remove::<PreviousStripPosition>()
                .insert((Position(display_bounds.min), SelectedVirtualMarker));
        }
        return;
    }

    let next_virtual_index = world
        .query::<(&LayoutStrip, Has<NativeFullscreenMarker>)>()
        .iter(world)
        .filter(|(strip, fullscreen)| !fullscreen && strip.id() == target_space_id)
        .map(|(strip, _)| strip.virtual_index)
        .max()
        .map_or(0, |max_idx| max_idx + 1);

    extracted.virtual_index = next_virtual_index;
    let focus = extracted.first().ok().and_then(|col| col.top());
    if next_virtual_index == 0 {
        world.spawn((
            Position(display_bounds.min),
            extracted,
            ChildOf(display_entity),
            SelectedVirtualMarker,
        ));
    } else {
        world.spawn((
            Position(display_bounds.max - PARKED_STRIP_SLIVER),
            PreviousStripPosition {
                origin: display_bounds.min,
                focus,
            },
            extracted,
            ChildOf(display_entity),
        ));
    }
}

/// Renumbers the rows of `workspace_id` from `0`, populated rows first, so a
/// Space with windows never shows an empty row 0. While the Space has a display
/// no row is removed: dropping empty rows is the reap or dynamic rule's job,
/// and both are off by default (WSD-8). A Space with no display loses its rows.
/// Native fullscreen rows are left alone.
pub(crate) fn compact_workspace_strips(world: &mut World, workspace_id: WorkspaceId) {
    let mut rows: Vec<(Entity, u32, usize, bool, bool, Option<Entity>)> = world
        .query::<(
            Entity,
            &LayoutStrip,
            Has<ActiveWorkspaceMarker>,
            Has<SelectedVirtualMarker>,
            Option<&ChildOf>,
            Has<NativeFullscreenMarker>,
        )>()
        .iter(world)
        .filter_map(|(entity, strip, active, selected, child, fullscreen)| {
            (!fullscreen && strip.id() == workspace_id).then_some((
                entity,
                strip.virtual_index,
                strip.len(),
                active,
                selected,
                child.map(ChildOf::parent),
            ))
        })
        .collect();

    if rows.is_empty() {
        return;
    }
    rows.sort_by_key(|(_, v_idx, _, _, _, _)| *v_idx);

    let has_populated = rows.iter().any(|(_, _, len, _, _, _)| *len > 0);
    let space_has_display = rows.iter().any(|(_, _, _, _, _, parent)| parent.is_some());

    if has_populated {
        // Populated rows first, keeping their order, then the empty ones.
        let (populated, empty): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.2 > 0);
        let had_active = empty.iter().any(|row| row.3);
        let had_selected = empty.iter().any(|row| row.4);
        let any_selected = populated.iter().any(|row| row.4);
        renumber_rows(world, populated.iter().chain(&empty).map(|row| row.0));
        // An empty row that was shown hands its markers to the first populated
        // one; the marker observers take them off the empty row.
        if (had_active || had_selected || !any_selected)
            && let Some(&(first_entity, .., parent)) = populated.first()
        {
            promote_strip_to_first(world, first_entity, parent, true, had_active);
        }
    } else if !space_has_display {
        for (entity, ..) in rows {
            world.despawn(entity);
        }
    } else {
        // Nothing populated: keep every row and show the first, as before.
        let had_active = rows[1..].iter().any(|row| row.3);
        let had_selected = rows[1..].iter().any(|row| row.4);
        renumber_rows(world, rows.iter().map(|row| row.0));
        let (first_entity, .., parent) = rows[0];
        promote_strip_to_first(world, first_entity, parent, had_selected, had_active);
    }
}

/// Numbers `rows` `0..n` in order, writing only the ones that change.
fn renumber_rows(world: &mut World, rows: impl Iterator<Item = Entity>) {
    for (index, entity) in (0u32..).zip(rows) {
        if let Some(mut strip) = world.get_mut::<LayoutStrip>(entity)
            && strip.virtual_index != index
        {
            strip.virtual_index = index;
        }
    }
}

fn promote_strip_to_first(
    world: &mut World,
    strip_entity: Entity,
    parent: Option<Entity>,
    make_selected: bool,
    make_active: bool,
) {
    if let Some(prev) = world
        .entity_mut(strip_entity)
        .take::<PreviousStripPosition>()
    {
        if let Some(mut pos) = world.get_mut::<Position>(strip_entity) {
            pos.0 = prev.origin;
        }
    } else if let Some(display_entity) = parent
        && let Some(bounds) = world.get::<Display>(display_entity).map(Display::bounds)
        && let Some(mut pos) = world.get_mut::<Position>(strip_entity)
    {
        pos.0 = bounds.min;
    }
    if make_selected {
        world.entity_mut(strip_entity).insert(SelectedVirtualMarker);
    }
    if make_active {
        world.entity_mut(strip_entity).insert(ActiveWorkspaceMarker);
    }
}

/// Rebase each strip onto its current display and ask the layout and OS writers
/// to replay window frames. This also covers wakeups where macOS moved windows
/// without changing the reported display geometry.
fn refresh_display_layout(world: &mut World, previous_parents: &HashMap<Entity, IRect>) {
    let strips = world
        .query_filtered::<Entity, With<LayoutStrip>>()
        .iter(world)
        .collect::<Vec<_>>();
    let focused = world
        .query_filtered::<Entity, With<FocusedMarker>>()
        .iter(world)
        .next();

    for strip_entity in strips {
        refresh_strip_layout(
            world,
            strip_entity,
            previous_parents.get(&strip_entity).copied(),
            focused,
        );
    }
}

/// Replays a strip that acquired a display after the initial reconciliation.
/// macOS may publish the space ownership later than the display itself.
pub(crate) fn refresh_reparented_strip(world: &mut World, strip_entity: Entity) {
    let focused = world
        .query_filtered::<Entity, With<FocusedMarker>>()
        .iter(world)
        .next();
    refresh_strip_layout(world, strip_entity, None, focused);
}

fn refresh_strip_layout(
    world: &mut World,
    strip_entity: Entity,
    previous_bounds: Option<IRect>,
    focused: Option<Entity>,
) {
    let Some(display_entity) = world.get::<ChildOf>(strip_entity).map(ChildOf::parent) else {
        return;
    };
    let Some(display_bounds) = world.get::<Display>(display_entity).map(Display::bounds) else {
        return;
    };
    if previous_bounds.is_none() {
        // A rescued orphan has no usable display-relative scroll offset.
        world.entity_mut(strip_entity).remove::<ManualStripOffset>();
    }
    let hidden = world.get::<PreviousStripPosition>(strip_entity).is_some();
    if let Some(mut position) = world.get_mut::<Position>(strip_entity) {
        let origin = if hidden {
            display_bounds.max - PARKED_STRIP_SLIVER
        } else if let Some(previous_bounds) = previous_bounds {
            position.0 + display_bounds.min - previous_bounds.min
        } else {
            display_bounds.min
        };
        if position.0 != origin {
            position.0 = origin;
        }
    }
    if let Some(mut previous) = world.get_mut::<PreviousStripPosition>(strip_entity) {
        previous.origin = previous_bounds.map_or(display_bounds.min, |bounds| {
            previous.origin + display_bounds.min - bounds.min
        });
    }
    if let Some(mut target) = world.get_mut::<RepositionMarker>(strip_entity) {
        target.0 = if hidden {
            display_bounds.max - PARKED_STRIP_SLIVER
        } else {
            previous_bounds.map_or(display_bounds.min, |bounds| {
                target.0 + display_bounds.min - bounds.min
            })
        };
    }

    let Some(mut strip) = world.get_mut::<LayoutStrip>(strip_entity) else {
        return;
    };
    let windows = strip.all_windows();
    strip.set_changed();
    if let Some(focused) = focused.filter(|focused| windows.contains(focused)) {
        world
            .entity_mut(focused)
            .insert(EnsureVisibleMarker { snap: true });
    }
    for window in windows {
        if let Some(mut layout) = world.get_mut::<LayoutPosition>(window) {
            layout.set_changed();
        }
        if let Some(mut position) = world.get_mut::<Position>(window) {
            position.set_changed();
        }
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(display_id))]
fn add_display(
    display: Display,
    workspace_ids: &[WorkspaceId],
    existing_strips: &Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    window_manager: &WindowManager,
    commands: &mut Commands,
) {
    let display_id = display.id();
    debug!("Display Added: {display_id}");

    let display_bounds = display.bounds();
    let display_entity = commands.spawn(display).id();
    commands.trigger(ReadDisplayProperties(display_entity));

    reparent_existing_workspaces(
        workspace_ids,
        display_entity,
        &display_bounds,
        existing_strips,
        window_manager,
        commands,
    );
}

#[instrument(level = Level::DEBUG, skip_all, fields(display_id))]
fn remove_display(
    display: &Display,
    workspaces: &Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    displays: &Query<(&mut Display, Entity)>,
    commands: &mut Commands,
) {
    let display_id = display.id();
    debug!("Display Removed: {display_id:?}");
    let Some((display, display_entity)) = displays
        .into_iter()
        .find(|(display, _)| display.id() == display_id)
    else {
        error!("Unable to find removed display!");
        return;
    };

    for (strip, entity, _) in workspaces
        .into_iter()
        .filter(|(_, _, child)| child.is_some_and(|child| child.parent() == display_entity))
    {
        let display_id = display.id();
        debug!(
            "orphaning strip {} after removal of display {display_id}.",
            strip.id(),
        );
        let timeout = Timeout::new(
            Duration::from_secs(ORPHANED_SPACES_TIMEOUT_SEC),
            Some(format!(
                "Orphaned strip {} ({strip}) could not be re-inserted after {ORPHANED_SPACES_TIMEOUT_SEC}s.",
                strip.id()
            )),
            commands,
        );
        if let Ok(mut commands) = commands.get_entity(entity) {
            commands.try_insert(timeout);
        }
        if let Ok(mut commands) = commands.get_entity(display_entity) {
            commands.detach_child(entity);
        }
    }

    if let Ok(mut commands) = commands.get_entity(display_entity) {
        commands.try_despawn();
    }
}

#[instrument(level = Level::DEBUG, skip_all, fields(display_id))]
fn move_display(
    moved_display: Display,
    workspace_ids: &[WorkspaceId],
    displays: &mut Query<(&mut Display, Entity)>,
    existing_strips: &Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    window_manager: &WindowManager,
    commands: &mut Commands,
) {
    let display_id = moved_display.id();
    debug!("Display Moved: {display_id:?}");
    let Some((mut display, display_entity)) = displays
        .iter_mut()
        .find(|(display, _)| display.id() == display_id)
    else {
        error!("Unable to find moved display!");
        return;
    };
    *display = moved_display;
    commands.trigger(ReadDisplayProperties(display_entity));

    reparent_existing_workspaces(
        workspace_ids,
        display_entity,
        &display.bounds(),
        existing_strips,
        window_manager,
        commands,
    );
}

fn reparent_existing_workspaces(
    workspace_ids: &[WorkspaceId],
    display_entity: Entity,
    display_bounds: &IRect,
    existing_strips: &Query<(&LayoutStrip, Entity, Option<&ChildOf>)>,
    window_manager: &WindowManager,
    commands: &mut Commands,
) {
    // Desktops first, so a new fullscreen Space's row can name one as its home.
    let mut spaces: Vec<_> = workspace_ids
        .iter()
        .map(|&id| (window_manager.space_is_fullscreen(id), id))
        .collect();
    spaces.sort_by_key(|&(fullscreen, _)| fullscreen);
    let mut home = None;

    // Verifies that a moved display has all the workspaces which it owns.
    for (fullscreen, id) in spaces {
        let mut row = None;
        for (strip, entity, child) in existing_strips {
            if strip.id() == id {
                if row.is_none() || strip.virtual_index == 0 {
                    row = Some(entity);
                }
                if child.is_none_or(|child| child.parent() != display_entity) {
                    // Re-parent this workspace
                    if let Ok(mut cmd) = commands.get_entity(entity) {
                        debug!("reparenting workspace {id} to display {display_entity}");
                        cmd.try_remove::<Timeout>()
                            .try_remove::<ChildOf>()
                            .try_insert(ChildOf(display_entity));
                    }
                }
            }
        }

        let row = row.unwrap_or_else(|| {
            debug!("new workspace {id} on display {display_entity}");
            let home = home.filter(|_| fullscreen);
            spawn_space_rows(
                commands,
                id,
                display_entity,
                display_bounds.min,
                false,
                home,
            )
        });
        if !fullscreen {
            home.get_or_insert((row, id));
        }
    }
}

/// Spawns what a Space starts with — row 0 and its floating layer — and
/// returns row 0. A fullscreen Space's row gets `home`: the row (and its
/// Space) its window returns to when it leaves fullscreen.
pub(crate) fn spawn_space_rows(
    commands: &mut Commands,
    space_id: WorkspaceId,
    display_entity: Entity,
    origin: Origin,
    active: bool,
    home: Option<(Entity, WorkspaceId)>,
) -> Entity {
    commands.spawn((FloatingLayer::new(space_id), ChildOf(display_entity)));
    let mut row = commands.spawn_layout_strip(
        LayoutStrip::new(space_id, 0),
        origin,
        display_entity,
        active,
    );
    if let Some((layout_strip, workspace_id)) = home {
        // usize::MAX: the end of the home row (insert_at clamps).
        row.insert(NativeFullscreenMarker {
            layout_strip,
            workspace_id,
            index: usize::MAX,
        });
    }
    row.id()
}

/// Tracks whether floating windows on a workspace sit above or behind tiled
/// ones in the OS z-order. Default is `Front` (floats above tiles).
#[derive(Clone, Component, Copy)]
pub struct FloatingLayer {
    pub workspace_id: WorkspaceId,
    pub front: bool,
}

impl FloatingLayer {
    pub fn new(workspace_id: WorkspaceId) -> Self {
        Self {
            workspace_id,
            front: false,
        }
    }

    pub fn flip(&mut self) {
        self.front = !self.front;
    }
}

fn read_display_properties_trigger(
    trigger: On<ReadDisplayProperties>,
    mut displays: Query<(&mut Display, Entity)>,
    platform: Option<NonSend<Pin<Box<PlatformCallbacks>>>>,
    config: Option<Res<Config>>,
    mut commands: Commands,
) {
    let Ok((mut display, entity)) = displays.get_mut(trigger.event().0) else {
        return;
    };
    let display_id = display.id();

    // NSScreen::screen needs to run in the main thread, thus we run it in a NonSend trigger.
    let Some(screens) = platform.map(|platform| NSScreen::screens(platform.main_thread_marker))
    else {
        return;
    };

    let notch = read_screen_property(&screens, display_id, |screen| {
        let insets = screen.safeAreaInsets();
        debug!("notch on display {display_id}: {insets:?}");
        round_px(insets.top)
    });
    if let Some(height) = notch {
        display.set_notch_height(height);
    }

    let dock = read_screen_property(&screens, display_id, |screen| {
        let visible_frame = irect_from(screen.visibleFrame());
        display.locate_dock(&visible_frame)
    });
    if let Some(dock) = dock {
        debug!("dock on display {display_id}: {:?}", dock);
        if let Ok(mut entity_commands) = commands.get_entity(entity) {
            entity_commands.try_insert(dock);
        }
    }

    if let Some(config) = config {
        let height = config.menubar_height();
        display.set_menubar_height_override(height);
    }
}
