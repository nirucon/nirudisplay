# NIRU Display editor reference

Detailed behaviour of the [NIRU Display](../README.md) window.

## The editor

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
  and rotation, or use the Single, Extend and Mirror layout actions. The mode,
  rate and rotation buttons open a list: modes sorted by size with the
  preferred mode marked and the current one checked, the rates of the chosen
  mode, and the four rotations. With a single enabled screen that screen is
  the primary one. Disabled
  outputs do not shift the active desktop origin, and unknown modes block Preview. Mirroring
  moves and edits the mirrored screens as one group.
- Identify every active screen with its number, connector and model for four
  seconds. Mirrored outputs are listed together on the physical screen.
- Save, load, rename, update and delete profiles. Saving keeps the edited draft
  and shows that it is saved but not applied; it never replaces the arrangement
  with the running layout. "Save as…" under the loaded profile's own name
  updates it; under another existing name it asks "Replace profile ‹name›?"
  first. Names take every character the keyboard layout types (Shift, AltGr,
  Å/Ä/Ö, the literal accent of a dead key). Renaming or deleting a startup
  profile updates its startup reference. Delete asks for confirmation.
- The profile list shows, per profile, a summary such as
  "3 screens · eDP + 2× 2560x1440", a **current** badge on the profile that
  matches the running layout and an **at login** badge on the startup profile.
  Profiles that need a screen that is not connected are dimmed and say what
  they need; they can still be renamed, deleted or used at login. Click loads
  a profile, a double-click loads and previews it. Screens and profiles are
  paged with the `<`/`>` buttons or the mouse wheel, so every entry stays
  reachable.
- Zoom with the mouse wheel over the canvas, pan with the middle mouse button,
  and press `0` to fit the layout again.
- Preview a layout for 15 seconds, keep it or revert, with a large countdown
  and a bar. A separate watchdog keeps the countdown working even if the window
  crashes or is killed. A preview token is adopted again when the window is
  reopened, so Keep and Revert keep working.
- Buttons, screens and profile rows show a hover state, and a disabled control
  says why when it is hovered or focused ("Keep is available during a preview").
- The window works down to 560x420: below 900x700 a compact layout stacks or
  scrolls the canvas, the panel and the profiles; below 560x420 it asks to be
  made larger.
- Opt in to a profile at the next login.
- **Automatic switching** (needs `niructl` from NIRUWM 0.17). The *Automatic*
  switch in the profile header turns it on or off; it is off by default.
  - **What it does.** With it on, the session applies the right layout when
    screens are plugged in or out or the laptop lid closes: a profile that
    fits exactly, else the layout these screens had last time, else the
    current layout with new screens to the right.
  - **Badges.** The row of the profile the automation applied carries an
    **auto** badge. A profile kept out of automatic switching
    (`auto = false`) says **manual**.
  - **Automatic layouts.** When the screens show a layout the automation
    made rather than a saved profile, the header says *Automatic layout
    (screens) — save as profile?* and *Save as…* is highlighted. It suggests
    the screens as the name.
  - **Header notes.** When several profiles fit equally well, the header
    names them and asks you to pick one; the one you pick is used next time.
    *Lid closed* and a running retry are noted there too.
- Profiles follow their screens by identity (make, model and serial). A
  profile that does not fit says why in its row: *needs HP E27q G5 (serial
  …2MRJ)*, *needs the built-in screen (the lid is closed)* or *also
  connected: HDMI-1*.

The monitor arrangement is deliberately separate from NIRUWM's tiling layouts.
The `layout` bar module still controls tile/monocle/floating, not which monitor
is where.

## Preview and profiles

`Preview` applies the draft with a visible countdown. `Keep` confirms it;
`Revert` or the timeout restores the previous layout. The timeout runs in a
separate `niructl` process, so a crash or kill of the window does not leave a
risky layout in place. Closing the window during a preview asks: **Keep
layout**, **Revert** or Cancel; closing never reverts silently. If the preview
expires while the draft has unsaved changes, the draft is kept and the status
line says so. Closing with unsaved changes asks first.

Every `niructl` call runs on one worker thread, one at a time, with a deadline
(20 s for preview, keep and revert, 5 s otherwise), so the window keeps
painting while a layout is applied; meanwhile only `Esc` is accepted, and it
closes the window.

## Events instead of polling

The window does not poll. It listens for RandR screen, output, CRTC and
property events on the root window, debounces them by 300 ms and reads
`niructl display status --json` only when the RandR configuration actually
changed, so the reprobe that `xrandr --query` inside niructl causes cannot
start another read. inotify on `~/.config/niruwm` and `~/.local/state/niruwm`
notices profiles and previews changed elsewhere and theme changes. During a
preview the countdown runs from a local deadline; the status is read again
every 5 s and just after the deadline, to see a Keep or Revert made elsewhere.
An idle open window starts no process.

`Use at login` stores an opt-in startup profile. `niruwm-session` applies it once
before the window manager starts. If a saved output is missing, the session logs
the problem and leaves the connected screens on the previous layout.

