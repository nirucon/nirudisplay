# Agent assignment: NIRUDISPLAY 0.3.0, a one-shot UX, bug and efficiency update

## 0. Quick start

1. Read this file, `README.md` and `src/window.rs`/`src/backend.rs`. The audit baseline
   is `f864a44` (0.2.2, pinned by NIRUX 0.12.199). Revalidate every item against
   current HEAD. If an item is already fixed, note that and move on.
2. Implement **D1–D10, then U1–U4 in order**. Commit per item or per small group, and keep
   `cargo fmt --check`, `clippy -D warnings`, tests and `tests/cli-smoke.sh` green
   after each commit.
3. Pass the gates in §4, release in §5 and report in §6.

This is an **execution** assignment. Do not stop after a plan, a branch or a draft
PR. Make sensible decisions, document them in the commit messages and the final report,
and continue without routine questions. Only ask if you hit a genuine external blocker or
a real risk of losing the owner's data.

## 1. Product invariants (must hold after every change)

- NIRUDISPLAY owns **no** profile store, xrandr parser or rollback timer. Every
  change still goes through `niructl display` (preview-json, keep, revert,
  save-json, rename, delete, startup). Do not change `niructl` unless an item says
  so. The CLI and the GUI must never disagree.
- The safety model stays: Preview → Keep/Revert, a 15 s watchdog in `niructl`, and token
  adoption after a crash or reopen.
- No new runtime dependency and no GTK/Qt. Use `x11rb` (the `randr` feature is already
  enabled), `ab_glyph`, `serde_json` and `libc`. Keep `rust-version = 1.85` and `--locked`
  builds working **with rustc 1.85**. Debian 13 ships 1.85.
- The look and the existing keyboard/mouse behavior stay, except where an item changes
  them on purpose.
- English UI text, as in the rest of the suite.

## 2. Bugs (verified on niruxwork 2026-11-04)

### D1: Polling reprobes the monitors every 2 s while the window is open (high)
`run()` (`src/window.rs` ~3925) calls `state.reload(false)` every 2 s, and every
1 s during a preview. Each reload runs `niructl display status --json`, which runs
**`xrandr --query` and `xrandr --verbose`**. Both force an output reprobe and an
EDID read over DDC. A 10 s live trace showed 5× niructl, 5× `xrandr --query` and
5× `xrandr --verbose`. A reprobe can make some monitors and docks flicker or stutter,
and it wakes the GPU. On top of that, the poll loop wakes every 250 ms.
**Fix:**
- `randr::select_input` on the root window for `SCREEN_CHANGE | OUTPUT_CHANGE |
  CRTC_CHANGE | OUTPUT_PROPERTY`.
- Reload only when an event arrives, debounced by about 300 ms. Before reloading, compare
  `get_screen_resources_current` `config_timestamp`/`timestamp` with the last values seen,
  so that the reprobe caused by the app's own status call cannot cause a reload loop.
- During a preview, run the countdown from a local deadline (`seconds_left` at adoption +
  `Instant`), repaint once per second, and reload on a RandR event or about every 5 s to
  catch keep/revert done elsewhere.
- With no pending work, `poll()` blocks without a timeout, except for the identify overlay
  deadline and the preview tick.
- The explicit `R` key still does a full reload.

**Done when:** an idle open window starts 0 processes over 60 s. Plugging a monitor in or
out, or running `xrandr` from a terminal, still updates the window within 1 s. A preview
countdown still runs and ends correctly.

### D2: Text input cannot type Shift symbols, Å/Ä/Ö or AltGr characters (high)
`Keys` keeps only level 0, and `text_key` only uppercases ASCII. With the Swedish layout,
typing `Hemma (dock)_Å@` into "Save as…" produced **`Hemma 9dock0-2`**, while the hint
under the field says "spaces and Swedish letters are allowed".
**Fix:** port the tested keyboard handling from `nirucon/nirulauncher` `src/main.rs`:
`keyboard_level` (Shift/MOD5 AltGr), `apply_caps` and `keyboard_symbol`, together with
their tests. Keep all keysyms per keycode, and refresh the table on `MappingNotify` and
XKB map changes. Map Latin-1 keysyms 0x20–0xff to chars. Treat the dead keys
`dead_tilde`, `dead_circumflex`, `dead_grave`, `dead_acute` and `dead_diaeresis` as their
literal characters, because there is no compose support. Shortcut keys (I, R, Q, 0, …)
must still match on the base keysym.
**Done when:** a unit test with a Swedish keymap fixture and a native Xvfb test with
`setxkbmap se` show that `Hemma (dock)_Å@ ~$` is entered exactly.

### D3: "Save as…" silently overwrites an existing profile
`niructl display save-json` replaces a profile with the same name (`retain`).
**Fix:** if the name exists, ask in the existing confirm dialog: "Replace profile
‹name›?". Saving under the currently loaded profile's name is "Update" and needs
no confirmation.

### D4: `R` throws away an edited draft and the undo history
`reload(true)` clears `undo` and replaces the draft without asking.
**Fix:** checkpoint the draft before a forced reload and do not clear `undo`. Then
`Ctrl+Z` restores the draft. Show "Reloaded. Ctrl+Z restores your draft." when the
draft was dirty.

### D5: Backend calls block the UI thread without a deadline
Preview, keep, revert, save, rename, delete and startup run `niructl` synchronously.
Applying a layout can take seconds while the outputs resync, and during that time the
window freezes and input queues up. Only save and delete show `busy`.
**Fix:** run each backend action on one worker thread, one action at a time. Keep `busy`
set, disable the action buttons, show a "Working…" state and accept only `Esc`. Bound each
call with a deadline using the same pattern as nirubar's `probe::run`: read stdout/stderr
without blocking, kill and reap on timeout. Use 20 s for preview/keep/revert and 5 s for
everything else. A late result must not overwrite a newer state.

### D6: The window breaks when it is tiled smaller than its minimum size
On `ConfigureNotify`, `app.w`/`app.h` are clamped to `MIN_WIDTH`/`MIN_HEIGHT` even though
the real window is smaller. niruwm tiles it anyway, so the content is cut off.
**Fix:** paint for the real size. Add a compact layout below the current minimum, with the
canvas on top and the panel and profiles stacked or scrollable, down to about 560x420. Below
that, show "Make the window larger" instead of clipped controls. Also set
`_NET_WM_WINDOW_TYPE_DIALOG` if niruwm then floats the window. Check niruwm's
rules and do not change niruwm.

### D7: The single-screen primary warning nags twice
"No enabled screen is set as primary." is shown both in the canvas banner and in the
status line, even when only one screen is enabled. Many laptops have no RandR primary set.
**Fix:** with exactly one enabled output, the draft treats it as primary, and Preview sends
`primary = true`. Show a non-blocking issue only once, in the banner.

### D8: The close handler interns an atom on every ClientMessage
It also does not check `data[0] == WM_DELETE_WINDOW`. **Fix:** intern
`WM_PROTOCOLS`/`WM_DELETE_WINDOW` once in `App::open` and compare both.

### D9: Closing during a preview reverts without asking, and the docs disagree
On close, `run()` calls `backend::revert()` when a preview is active. The help text says
so, but the README says that closing does not end the preview. A user who is happy with a
preview and closes the window loses it.
**Fix:** when the user closes during a preview, use the confirm dialog with **Keep layout**
/ **Revert** / Cancel. A crash or kill still reverts through the watchdog. Correct the
README. Also fix the help text: "Screens: drag the < > buttons" should say "click".

### D10: Profiles that cannot be used on this hardware look normal
Clicking one gives the error "X is not connected". **Fix:**
- Dim profiles whose outputs are not all connected, and show "needs DisplayPort-7,
  DisplayPort-8".
- Mark the profile that matches the running layout as **current**, and the startup
  profile as **at login**.
- Show a short summary per profile, for example "3 screens · eDP + 2× 2560x1440".

## 3. UX and functionality

### U1: Pick mode, rate and rotation from a list
`Mode -/+`, `Rate -/+` and `Rotate` only cycle, so the user cannot see the options.
Add a popover list: modes sorted by size with the preferred mode marked and the current
mode checked, rates for the chosen mode, and rotation as four entries. Keyboard: Enter
opens, arrows move, Enter chooses, Esc closes. Keep the left/right shortcuts on focused
buttons. For mirror groups, the list contains only the common modes and rates, as before.

### U2: A clearer preview countdown
While a preview is active, show the countdown as a large label with a progress bar.
**Keep** is the default, highlighted button, and Revert sits next to it. Enter = Keep and
Esc = Revert, but only while a preview is active and has started from this window, and the
footer must say so.

### U3: The profile area
Use the empty space: a list with the D10 badges and summaries, a double-click to load +
Preview, and the Delete, Rename and "Use at login" actions per selected row. Rows that
cannot be reached by paging stay unchanged.

### U4: Hover feedback
Show a hover state on buttons, screens and profile rows, plus a short inline explanation
when a disabled button is hovered or focused. For example: "Extend needs two enabled
screens", "Keep is available during a preview".

Out of scope: automatic dock/EDID profile switching, scaling/DPI, HDR/VRR, Wayland,
and changes to the `niructl` JSON schema.

## 4. Gates

Automated: `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked`, a release build and `sh tests/cli-smoke.sh`. Also:
- **Add an MSRV job to CI** (`dtolnay/rust-toolchain@1.85`, run `cargo build --locked`)
  so that a lockfile bump can never again require a newer rustc than `rust-version`.
- New tests: the D2 keymap fixture, the D1 debounce and timestamp loop guard (pure logic),
  D3 overwrite detection, D4 undo after reload, D7 single-output primary, the D10
  profile availability/current matching, the D6 compact layout without control
  collisions at 560x420 (extend the existing collision test), and U1 popover keyboard
  navigation.
- Add an Xvfb x11 test, ignored by default and run in CI like nirubar's: open the window,
  type into "Save as…" with `setxkbmap se`, and assert the result through a
  `NIRUDISPLAY_NIRUCTL` fixture that records stdin and argv. Use the existing env override.

Native:
- **niruxwork** (the owner's work laptop, one eDP; the owner allows local testing): open,
  idle 60 s and trace with `strace -f -e execve` (0 spawns expected). Do the Save-as text
  test, run a preview, Keep and Revert of the *current* layout, and test the close-during-
  preview dialog. Leave the display exactly as it was. Do not save or delete the owner's
  profiles; use a temporary profile name and delete it afterwards.
- **othala** (Debian, 2×2560x1440, owner-authorized for full testing and re-login): drag,
  snap, Extend, Mirror and Single, mode/rate/rotation popovers, preview, keep and revert,
  output disable/enable via RandR, the D1 reaction to an `xrandr` change from a terminal,
  and the D6 compact layout when tiled. Restore the original topology and `displays.toml`
  afterwards (back them up first).
- Credentials, if any are needed, are supplied privately. Never commit them.

## 5. Release

Bump to **0.3.0**. Update the README to match the code (D9, U1–U4, the D1 event
model), tag `v0.3.0` through the normal release workflow and verify the assets and
checksums. Then pin it in `nirucon/nirux` `manifest`:
- If `nirux/tools/suite-pin` exists, use it.
- Otherwise bump the manifest and suite version the usual way. Change only the nirudisplay
  line, and rebase if other pin commits land at the same time.

Install through `nirux update` on niruxwork and othala, and verify that the running
`/proc/<pid>/exe` comes from the new generation. Do not move or replace published
tags or assets.

## 6. Final report

For each item D1–D10 and U1–U4: what changed, how it was tested, and the evidence (trace
counts before and after, test names, native observations). Include release URLs, the
installed and running version on each host, and any remaining limitations. If something
could not be completed, mark it **PARTIAL** and give the exact remaining steps.
