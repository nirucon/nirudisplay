# NIRU Display

**Part of [NIRUX - a minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

A lightweight graphical monitor editor for the NIRUX desktop. It edits a draft,
shows the screens as they are arranged, and sends every change through the same
`niructl display` transaction as the CLI and launcher. It owns no profile store,
no xrandr parser and no rollback daemon.

## What it does

- Drag screens to move them, with edge snapping, or use the keyboard and exact
  coordinate fields.
- Enable or disable connected outputs, choose the primary screen, resolution,
  refresh rate and rotation.
- Identify every active screen with a number.
- Save, load, rename, update and delete profiles in
  `~/.config/niruwm/displays.toml`.
- Preview a layout for 15 seconds, keep it or revert. A separate watchdog keeps
  the countdown working even if the window is closed or crashes.
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

Keyboard: `Tab` moves focus, arrows move the selected screen (`Shift` for 10 px),
`Space`/`Enter` activates the focused control, `I` identifies screens, `R`
reloads, `F1` shows help and `Esc`/`Q` closes.

## Preview and profiles

`Preview` applies the draft with a visible countdown. `Keep` confirms it;
`Revert` or the timeout restores the previous layout. The timeout runs in a
separate `niructl` process, so closing the window does not leave a risky layout
in place. Profile files keep older entries without a refresh rate working;
new saves record the rate that was actually applied.

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
