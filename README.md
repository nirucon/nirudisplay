# NIRU Display

**Part of [NIRUX - a minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

A lightweight graphical monitor editor for the NIRUX desktop. It edits a draft,
shows the screens as they are arranged, and sends every change through the same
`niructl display` transaction as the CLI and launcher. It owns no profile store,
no xrandr parser and no rollback daemon.

## What it does

- Drag screens to move them. A frozen canvas, a dead zone and pixel-based edge
  snapping keep the screen under the pointer while dragging. The exact position
  is shown next to the screen and snap guides show shared edges. Arrow keys move
  the selected screen by 1 px, or 10 px with Shift.
- Every screen has a stable number, a full frame and a distinct fill. The number
  is the same in the canvas, the screen list and Identify. Connector, EDID
  manufacturer/model and a serial suffix make identical monitors distinguishable.
- A selected screen has its own ring; the primary screen has a separate `P`
  badge. Disabled outputs stay visible with a dashed frame. Partial overlaps and
  invalid layouts are drawn in the warning colour and block Preview until fixed.
- Enable or disable outputs, choose the primary screen, resolution, refresh rate
  and rotation, or use the Single, Extend and Mirror layout actions. Disabled
  outputs do not shift the active desktop origin, and unknown modes block Preview. Mirroring
  moves and edits the mirrored screens as one group.
- Identify every active screen with its number, connector and model for four
  seconds. Mirrored outputs are listed together on the physical screen.
- Save, load, rename, update and delete profiles. Saving keeps the edited draft
  and shows that it is saved but not applied; it never replaces the arrangement
  with the running layout. Renaming or deleting a startup profile updates its
  startup reference. Delete asks for confirmation. Screens and profiles are paged
  with the `<`/`>` buttons or the mouse wheel, so every entry stays reachable.
- Zoom with the mouse wheel over the canvas, pan with the middle mouse button,
  and press `0` to fit the layout again.
- Preview a layout for 15 seconds, keep it or revert. A separate watchdog keeps
  the countdown working even if the window is closed or crashes. A preview token
  is adopted again when the window is reopened, so Keep and Revert keep working.
- Opt in to a profile at the next login; nothing is applied automatically by
  default.

The monitor arrangement is deliberately separate from NIRUWM's tiling layouts.
The `layout` bar module still controls tile/monocle/floating, not which monitor
is where.

## Usage

```sh
nirudisplay              # open the window
nirudisplay status       # connected screens and saved profiles
nirudisplay list         # saved profiles
nirudisplay --version | --help
```

Keyboard: `Tab` moves focus, arrows move the selected screen (`Shift` for 10 px)
when no property control is focused, `Space`/`Enter` activates the focused
control, `I` identifies screens, `R` reloads, `Ctrl+Z` undoes a draft change, `0` fits the canvas,
`F1` shows help and `Esc`/`Q` closes. With a mode or rate button focused,
left/right change that property.

## Preview and profiles

`Preview` applies the draft with a visible countdown. `Keep` confirms it;
`Revert` or the timeout restores the previous layout. The timeout runs in a
separate `niructl` process, so closing the window does not leave a risky layout
in place. If the preview expires while the draft has unsaved changes, the draft
is kept and the status line says so. Closing with unsaved changes asks first.

`Use at login` stores an opt-in startup profile. `niruwm-session` applies it once
before the window manager starts. If a saved output is missing, the session logs
the problem and leaves the connected screens on the previous layout.

## Scope

Resolution, refresh rate, position, rotation, primary, mirror/extend/single and
profile management are in v1. Automatic dock/EDID profile switching, arbitrary
modelines, fractional scaling, ICC calibration, DDC/CI, HDR/VRR and Wayland are
future work. The window does not advertise controls the current X session cannot
provide.

## Build

```sh
cargo build --release --locked
cargo test --locked
sh tests/cli-smoke.sh target/release/nirudisplay
```

The dependencies are already used across the suite: `x11rb`, `ab_glyph` and
`serde_json`. There is no GTK, Qt, Electron or WebView runtime requirement.

## Files

| Path | Purpose |
|---|---|
| `~/.config/niruwm/displays.toml` | Profiles and the optional startup profile, owned by `niructl` |
| `~/.local/state/niruwm/display-preview.toml` | Pending preview transaction, owned by `niructl` |
| `NIRUDISPLAY_NIRUCTL` | Development override for a test `niructl` binary |

MIT. Copyright 2026 Nicklas Rudolfsson.
