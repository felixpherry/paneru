# Record

**Overview input.** While the overview is open the `CGEventTap` in `src/platform/input.rs` routes bare Escape, Return and arrow keys to the overview, runs Paneru keybindings as usual, passes every other command, option or control chord through to macOS and other applications, and swallows the remaining bare keys. Selection is the real focus: focus and workspace moves made while the overview is open act on the live layout behind it, and closing leaves focus where it is. A click is taken by the overview only when the overview window is under the pointer.

**Overview backdrop.** The overview draws an opaque backdrop — the display's desktop wallpaper, else the scrim colour — so no application window shows through it.

**Screen capture.** Overview tiles carry window thumbnails captured through ScreenCaptureKit on macOS 14 and later, which requires the Screen Recording permission. This is the only feature in Paneru that needs a permission beyond Accessibility; when it is unavailable, tiles fall back to the application icon and window title and nothing else degrades.

**Dynamic workspaces.** With `dynamic_workspaces` enabled, each macOS Space numbers its virtual workspaces 1..N without gaps: a workspace with no managed windows is removed as soon as no display is showing it, and exactly one empty workspace always follows the last occupied one. Switches and moves by number, `last` or southward never go past that trailing workspace. Native fullscreen Spaces are exempt.

**Floating round trip.** Tiling a floating window back inserts it as a new column right of the active column — the last-focused tiled window on the active row — or appends it when that row has none. Floating the active column hands that role to the column now at its index, or to its left neighbour if it was last; a window that was just opened or tiled back hands it back to the column it was inserted beside. A window floats again at its last floating size and position, kept proportional to the display and fully on screen; a `grid` rule places only its first float.

**Leaving focus.** When the focused window leaves its row — it closes, its application quits, it is hidden or minimized, or it moves to another display or virtual workspace — focus follows the same niri rule as floating the active column: the next window down its stack, else up; otherwise the column now at its index, or its left neighbour if it was last. When focus moves along with the window, the row it left remembers that same successor. A window sent to another display from a row that isn't shown keeps focus, and that row remembers the successor. A window that was just opened or tiled back beside a column hands focus back to that column, except when it moves to another virtual workspace. A floating window that closes hands focus to the active column; one that is hidden or minimized leaves the choice to macOS. A native tab that leaves its row leaves the next tab to its application. When the last window on a display closes, focus goes to the active column of another display's shown row. A window that leaves its row without focus leaves focus where it is, unless it lost focus less than half a second before.

**Overview layout.** The overview draws every virtual workspace row of the active display at one shared zoom (`[overview] zoom`), with the active row centred vertically and the others stacked above and below it. Each row is scrolled so its centre column — the focused window on the active row, the row's remembered focus elsewhere — sits in the horizontal centre, which is the window ↑/↓ lands on. Whenever the layout changes while the overview is open, tiles slide from where they are drawn to their new place.

**Display moves.** `window display <dir>` moves the focused tiled window, with its tab group, to the nearest display in that direction and focus follows it; `window nextdisplay` does the same toward the first other display. The window lands as a new column right of the target row's active column, or alone when that row is empty. Floating windows do not move between displays.

**Saved state.** Paneru does not write its state file while it knows no display, while any display has no rows yet, or while startup or session restore is still running, so an empty or half-built world never replaces the saved layout.

**Startup displays.** When the startup display scan fails, misses a display, or can't read the current Space, Paneru retries the scan. A display whose Spaces still can't be read is kept without rows, and its rows are built as soon as a complete scan can read it.

**Dead Spaces.** A virtual workspace row whose macOS Space no display lists any more is removed once two complete display scans in a row, a second apart, leave it out. A scan is complete only when every display's Spaces were read, its displays are the ones Paneru knows, and no Space is listed twice; an incomplete scan never removes rows and restarts the count.

**Stray windows.** A managed window that ends up in no row — its desktop was removed, its display unplugged, its row removed as a dead Space, or it left native fullscreen — is tiled into the shown row of the Space macOS lists it on, else that Space's lowest row, once it has been in no row for two passes a second apart. A window just tiled where a binding was pressed waits until its move settles. A window is left alone while that row is a fullscreen row or no Space lists it. A window found in native fullscreen at startup or in a restored session goes straight back to its desktop row when it leaves fullscreen.

**Unreadable Spaces.** When macOS can't list a Space's windows, Lua snapshots, queries and broadcasts still report the whole world with a log line; only that Space's floating windows are missing.

**Display focus.** Focusing another display lands on the window last focused there, or on its widest visible window when it has none.

**Overfull stacks.** When a stacked column cannot give every window its minimum height, its windows share the column height evenly.

**Install signing.** The install script signs Paneru with a stable self-signed identity, so macOS keeps its Accessibility and Screen Recording grants across rebuilds.
