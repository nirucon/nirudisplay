//! The NIRU Display window.
//!
//! The window is a front end for `niructl display`. It edits a local draft and
//! sends that draft through the same preview/keep/revert transaction the CLI
//! and the launcher use. It owns no profile store and runs no display daemon.

use crate::{backend, keyboard, text, theme, watch};
use std::time::{Duration, Instant};
use x11rb::{
    connection::Connection,
    protocol::{
        randr::{self, ConnectionExt as _},
        xproto::*,
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT,
};

pub const APP_ID: &str = "nirudisplay";
pub const APP_NAME: &str = "NIRU Display";

const WIDTH: u16 = 1120;
const HEIGHT: u16 = 760;
/// Below this size the compact layout takes over.
const MIN_WIDTH: u16 = 900;
const MIN_HEIGHT: u16 = 700;
/// D6: the smallest size the compact layout supports, and the minimum the
/// window advertises. Below it the window asks to be made larger.
const SMALLEST_WIDTH: u16 = 560;
const SMALLEST_HEIGHT: u16 = 420;
const HEADER_COMPACT: i16 = 52;
/// The panel at its shortest: two screen rows and every control.
const PANEL_MIN_H: i16 = 28 + 2 * ROW_H + 8 + 128 + 52 + 38;

const PAD: i16 = 20;
const HEADER: i16 = 76;
const FOOTER: i16 = 58;
const PANEL_W: i16 = 330;
const PROFILE_H: i16 = 164;
const ROW_H: i16 = 30;
/// U3: one profile row: the name with its badges, then the summary.
const PROFILE_ROW_H: i16 = 30;
const PROFILE_ROW_GAP: i16 = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

const K_ESCAPE: u32 = 0xff1b;
const K_TAB: u32 = 0xff09;
const K_RETURN: u32 = 0xff0d;
const K_BACKSPACE: u32 = 0xff08;
const K_LEFT: u32 = 0xff51;
const K_UP: u32 = 0xff52;
const K_RIGHT: u32 = 0xff53;
const K_DOWN: u32 = 0xff54;
const K_PAGEUP: u32 = 0xff55;
const K_PAGEDOWN: u32 = 0xff56;
const K_HOME: u32 = 0xff50;
const K_END: u32 = 0xff57;
const K_SPACE: u32 = 0x0020;
const K_KP_ENTER: u32 = 0xff8d;
const K_F1: u32 = 0xffbe;
const K_Q: u32 = b'q' as u32;
const K_I: u32 = b'i' as u32;
const K_R: u32 = b'r' as u32;
const K_Z: u32 = b'z' as u32;
const K_S: u32 = b's' as u32;
const K_K: u32 = b'k' as u32;
const K_0: u32 = b'0' as u32;
const K_N: u32 = b'n' as u32;
const K_Y: u32 = b'y' as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Monitor(usize),
    Output(usize),
    Enabled,
    Primary,
    Rotate,
    /// U1: the mode and rate buttons open a list.
    Mode,
    Rate,
    /// A row of the open mode/rate/rotation list.
    PopItem(usize),
    Identify,
    Single,
    Extend,
    Mirror,
    Apply,
    Keep,
    Revert,
    Save,
    Rename,
    Delete,
    Startup,
    Reload,
    OutputPrev,
    OutputNext,
    Profile(usize),
    ProfilePrev,
    ProfileNext,
    /// Automatic switching on or off.
    Automatic,
    NameField,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftOutput {
    pub name: String,
    pub connected: bool,
    pub enabled: bool,
    pub mode: String,
    pub rate: Option<f64>,
    pub x: i32,
    pub y: i32,
    pub rotation: String,
    pub primary: bool,
}

impl DraftOutput {
    pub fn size(&self) -> (i32, i32) {
        let (mut width, mut height) = mode_size(&self.mode).unwrap_or((1920, 1080));
        if matches!(self.rotation.as_str(), "left" | "right") {
            std::mem::swap(&mut width, &mut height);
        }
        (width, height)
    }

    pub fn width(&self) -> i32 {
        self.size().0
    }

    pub fn height(&self) -> i32 {
        self.size().1
    }

    fn same_layout(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.x == other.x
            && self.y == other.y
            && self.mode == other.mode
            && self.rotation == other.rotation
            && match (self.rate, other.rate) {
                (Some(left), Some(right)) => (left - right).abs() < 0.01,
                (None, None) => true,
                _ => false,
            }
    }

    fn mirrors(&self, other: &Self) -> bool {
        self.enabled && other.enabled && self.same_layout(other)
    }

    fn same_state(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.primary == other.primary
            && self.x == other.x
            && self.y == other.y
            && self.mode == other.mode
            && self.rotation == other.rotation
            && match (self.rate, other.rate) {
                (Some(left), Some(right)) => (left - right).abs() < 0.01,
                (None, None) => true,
                _ => false,
            }
    }
}

fn layouts_match(left: &[DraftOutput], right: &[DraftOutput]) -> bool {
    left.len() == right.len()
        && left.iter().all(|output| {
            right
                .iter()
                .find(|candidate| candidate.name == output.name)
                .map(|candidate| output.same_state(candidate))
                .unwrap_or(false)
        })
}

fn mode_size(mode: &str) -> Option<(i32, i32)> {
    let (width, rest) = mode.split_once('x')?;
    let width = leading_number(width)?;
    let height = leading_number(rest)?;
    (width > 0 && height > 0).then_some((width, height))
}

fn leading_number(text: &str) -> Option<i32> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameAction {
    Save,
    Rename,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    Close,
    Delete(String),
    /// D3: "Save as…" with the name of another existing profile.
    Replace(String),
    /// D9: the window is closed while a preview runs.
    ClosePreview,
}

impl Confirm {
    /// The dialog's buttons, the default first and Cancel last.
    pub fn buttons(&self) -> &'static [&'static str] {
        match self {
            Confirm::Close => &["Close", "Cancel"],
            Confirm::Delete(_) => &["Delete", "Cancel"],
            Confirm::Replace(_) => &["Replace", "Cancel"],
            Confirm::ClosePreview => &["Keep layout", "Revert", "Cancel"],
        }
    }
}

pub const ROTATIONS: [&str; 4] = ["normal", "left", "inverted", "right"];
/// Rows a mode/rate list shows at most.
const POP_ROWS: usize = 10;
const POP_ROW_H: i16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopKind {
    Mode,
    Rate,
    Rotation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PopValue {
    Mode(backend::DisplayMode),
    Rate(f64),
    Rotation(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PopItem {
    pub label: String,
    pub note: Option<&'static str>,
    pub checked: bool,
    pub value: PopValue,
}

/// U1: an open mode, rate or rotation list.
#[derive(Clone, Debug, PartialEq)]
pub struct Popover {
    pub kind: PopKind,
    pub items: Vec<PopItem>,
    pub cursor: usize,
    pub scroll: usize,
}

impl Popover {
    /// Keep the cursor among the visible rows.
    fn reveal(&mut self) {
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + POP_ROWS {
            self.scroll = self.cursor + 1 - POP_ROWS;
        }
    }
}

/// What the window does with a finished backend job. It travels to the
/// worker with the job and back with the answer.
#[derive(Clone, Debug, PartialEq)]
pub enum After {
    /// A status read. `forced` replaces an edited draft (the `R` key);
    /// `background` reads are the window's own reaction to a change and
    /// never make the window busy.
    Reload {
        forced: bool,
        background: bool,
    },
    Preview {
        layout: Vec<DraftOutput>,
    },
    Keep {
        quit: bool,
    },
    Revert {
        quit: bool,
    },
    Save {
        name: String,
        update: bool,
    },
    Rename {
        old: String,
        new: String,
    },
    Delete {
        name: String,
    },
    Startup {
        enabled: bool,
    },
    Automatic {
        enabled: bool,
    },
}

impl After {
    fn background(&self) -> bool {
        matches!(
            self,
            After::Reload {
                background: true,
                ..
            }
        )
    }

    fn busy_text(&self) -> &'static str {
        match self {
            After::Reload { .. } => "Reading the display state…",
            After::Preview { .. } => "Applying the layout…",
            After::Keep { .. } => "Keeping the layout…",
            After::Revert { .. } => "Restoring the previous layout…",
            After::Save { .. } => "Saving the profile…",
            After::Rename { .. } => "Renaming the profile…",
            After::Delete { .. } => "Deleting the profile…",
            After::Startup { .. } => "Updating the login profile…",
            After::Automatic { enabled: true } => "Turning automatic switching on…",
            After::Automatic { enabled: false } => "Turning automatic switching off…",
        }
    }
}

/// How long a preview asks the watchdog for.
pub const PREVIEW_SECONDS: u64 = 15;

pub struct State {
    pub status: backend::Status,
    pub draft: Vec<DraftOutput>,
    pub selected: usize,
    pub selected_profile: Option<usize>,
    pub focus: Option<Hit>,
    pub message: String,
    pub message_is_error: bool,
    /// An action is running on the worker; only Esc is accepted.
    pub busy: bool,
    pub busy_text: &'static str,
    pub dirty: bool,
    pub name_mode: Option<NameAction>,
    pub name_text: String,
    pub output_scroll: usize,
    pub profile_scroll: usize,
    pub confirm: Option<Confirm>,
    pub popover: Option<Popover>,
    pub quit: bool,
    /// Jobs for the worker, taken by the event loop.
    pub outbox: Vec<(backend::Job, After)>,
    /// Actions submitted and not answered yet.
    in_flight: usize,
    /// The sequence number of the newest status applied. An older answer
    /// never replaces it.
    status_seq: u64,
    /// The running preview was started from this window.
    pub own_preview: bool,
    preview_token: Option<String>,
    preview_deadline: Option<Instant>,
    preview_total: u64,
    had_preview: bool,
    preview_layout: Option<Vec<DraftOutput>>,
    undo: Vec<Vec<DraftOutput>>,
}

impl State {
    pub fn new(status: backend::Status) -> Self {
        let draft = draft_from_status(&status);
        Self {
            status: backend::Status::default(),
            draft,
            selected: 0,
            selected_profile: None,
            focus: None,
            message: String::new(),
            message_is_error: false,
            busy: false,
            busy_text: "",
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            popover: None,
            quit: false,
            outbox: Vec::new(),
            in_flight: 0,
            status_seq: 0,
            own_preview: false,
            preview_token: None,
            preview_deadline: None,
            preview_total: PREVIEW_SECONDS,
            had_preview: false,
            preview_layout: None,
            undo: Vec::new(),
        }
        .with_status(status)
    }

    fn with_status(mut self, status: backend::Status) -> Self {
        let had_preview = status.preview.is_some();
        self.message = status
            .preview
            .as_ref()
            .map(|preview| {
                format!(
                    "Previewing {}. Keep within {} s or the previous layout returns.",
                    preview.name, preview.seconds_left
                )
            })
            .unwrap_or_else(|| {
                "Drag a screen to move it. Changes are previewed before they are kept.".into()
            });
        self.had_preview = had_preview;
        self.preview_layout = had_preview.then(|| self.draft.clone());
        self.status = status;
        self.sync_preview_clock(Instant::now());
        self
    }

    pub fn load() -> Result<Self, String> {
        let status = backend::read_status()?;
        backend::adopt_preview(status.preview.as_ref());
        Ok(Self::new(status))
    }

    pub fn set_message(&mut self, text: impl Into<String>, error: bool) {
        self.message = text.into();
        self.message_is_error = error;
    }

    pub fn current(&self) -> Option<&DraftOutput> {
        self.draft.get(self.selected)
    }

    pub fn preview_active(&self) -> bool {
        self.status.preview.is_some()
    }

    pub fn preview_changed(&self) -> bool {
        self.preview_layout
            .as_ref()
            .map(|layout| !layouts_match(layout, &self.draft))
            .unwrap_or(false)
    }

    /// The preview countdown runs from a local deadline, set when the
    /// preview is first seen and corrected by later status reads, so the
    /// window repaints once per second without asking niructl.
    fn sync_preview_clock(&mut self, now: Instant) {
        let Some(preview) = &self.status.preview else {
            self.preview_token = None;
            self.preview_deadline = None;
            self.own_preview = false;
            return;
        };
        let target = now + Duration::from_secs(preview.seconds_left);
        if self.preview_token.as_deref() != Some(preview.token.as_str()) {
            self.preview_token = Some(preview.token.clone());
            self.preview_deadline = Some(target);
            self.preview_total = preview.seconds_left.max(PREVIEW_SECONDS);
            self.own_preview = false;
        } else if let Some(deadline) = self.preview_deadline {
            let drift = if deadline > target {
                deadline - target
            } else {
                target - deadline
            };
            if drift > Duration::from_millis(1500) {
                self.preview_deadline = Some(target);
            }
        }
    }

    /// Whole seconds left in the preview, rounded up like a countdown.
    pub fn preview_seconds_left(&self, now: Instant) -> Option<u64> {
        self.status.preview.as_ref()?;
        let left = self
            .preview_deadline?
            .saturating_duration_since(now)
            .as_millis();
        Some(left.div_ceil(1000) as u64)
    }

    /// The share of the preview time that is left, 0.0 to 1.0.
    pub fn preview_fraction(&self, now: Instant) -> f32 {
        let Some(deadline) = self.preview_deadline else {
            return 0.0;
        };
        let left = deadline.saturating_duration_since(now).as_secs_f32();
        (left / self.preview_total.max(1) as f32).clamp(0.0, 1.0)
    }

    pub fn preview_deadline(&self) -> Option<Instant> {
        self.preview_deadline
    }

    fn refresh_dirty_from_status(&mut self) {
        let actual = draft_from_status(&self.status);
        self.dirty = !layouts_match(&self.draft, &actual);
    }

    /// Queue a job for the worker. Actions make the window busy; a
    /// background status read does not.
    pub fn submit(&mut self, job: backend::Job, after: After) {
        if !after.background() {
            self.in_flight += 1;
            self.busy = true;
            self.busy_text = after.busy_text();
        }
        self.outbox.push((job, after));
    }

    /// Ask for a status read; `forced` replaces an edited draft (`R`).
    pub fn request_reload(&mut self, forced: bool) {
        self.submit(
            backend::Job::Status,
            After::Reload {
                forced,
                background: false,
            },
        );
    }

    /// Apply one answer from the worker.
    pub fn finish(&mut self, done: backend::Done<After>) {
        if !done.after.background() {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.busy = self.in_flight > 0;
        }
        let status = match done.status {
            // The worker answers in order, but a result that is older than
            // the status on screen is still never applied.
            Ok(_) if done.seq < self.status_seq => None,
            Ok(status) => {
                self.status_seq = done.seq;
                Some(status)
            }
            Err(err) => {
                self.set_message(err, true);
                None
            }
        };
        let failed = match done.result {
            Ok(()) => false,
            Err(err) => {
                self.set_message(err, true);
                true
            }
        };
        match done.after {
            After::Reload { forced, background } => {
                if let Some(status) = status {
                    // D4: R keeps the edited draft one Ctrl+Z away.
                    let restorable = forced && !background && self.dirty;
                    if restorable {
                        self.checkpoint();
                    }
                    self.apply_status(status, forced);
                    if forced && !background {
                        self.set_message(
                            if restorable {
                                "Reloaded. Ctrl+Z restores your draft."
                            } else {
                                "Reloaded the display state."
                            },
                            false,
                        );
                    }
                }
            }
            After::Preview { layout } => {
                if !failed {
                    self.dirty = false;
                    self.had_preview = true;
                    self.preview_layout = Some(layout);
                    self.set_message(
                        format!(
                            "Previewing for {PREVIEW_SECONDS} s. Keep the result or let it revert."
                        ),
                        false,
                    );
                }
                if let Some(status) = status {
                    self.apply_status(status, false);
                }
                if !failed && self.preview_active() {
                    self.own_preview = true;
                }
            }
            After::Keep { quit } | After::Revert { quit } => {
                let kept = matches!(done.after, After::Keep { .. });
                if !failed {
                    self.dirty = false;
                    self.had_preview = false;
                    self.preview_layout = None;
                    self.own_preview = false;
                    self.set_message(
                        if kept {
                            "Display layout kept."
                        } else {
                            "Previous layout restored."
                        },
                        false,
                    );
                    self.quit |= quit;
                }
                if let Some(status) = status {
                    self.apply_status(status, !failed);
                }
            }
            After::Save { name, update } => {
                if let Some(status) = status {
                    self.replace_status(status);
                    self.refresh_dirty_from_status();
                }
                if !failed {
                    self.set_message(
                        if update {
                            format!("Updated display profile {name}.")
                        } else {
                            format!("Saved display profile {name}.")
                        },
                        false,
                    );
                    self.select_profile_named(&name);
                }
            }
            After::Rename { old, new } => {
                if let Some(status) = status {
                    self.replace_status(status);
                }
                if !failed {
                    self.set_message(format!("Renamed {old} to {new}."), false);
                    self.select_profile_named(&new);
                }
            }
            After::Delete { name } => {
                if let Some(status) = status {
                    self.replace_status(status);
                    self.refresh_dirty_from_status();
                }
                if !failed {
                    self.selected_profile = None;
                    self.set_message(format!("Deleted display profile {name}."), false);
                }
            }
            After::Automatic { enabled } => {
                if let Some(status) = status {
                    self.apply_status(status, false);
                }
                if !failed {
                    self.set_message(
                        if enabled {
                            "Automatic switching on: the layout follows the connected screens and the lid."
                        } else {
                            "Automatic switching off: layouts change only when you apply them."
                        },
                        false,
                    );
                }
            }
            After::Startup { enabled } => {
                if let Some(status) = status {
                    self.apply_status(status, false);
                }
                if !failed {
                    self.set_message(
                        if enabled {
                            "This profile will be applied at the next login."
                        } else {
                            "Startup profile disabled."
                        },
                        false,
                    );
                }
            }
        }
    }

    fn select_profile_named(&mut self, name: &str) {
        self.selected_profile = self
            .status
            .profiles
            .iter()
            .position(|profile| profile.name == name);
        if let Some(index) = self.selected_profile {
            self.profile_scroll = index;
        }
    }

    /// Take a new status. An edited draft is kept unless `force_draft`.
    pub fn apply_status(&mut self, status: backend::Status, force_draft: bool) {
        let was_preview = self.had_preview;
        let active = status.preview.is_some();
        if force_draft || !self.dirty {
            self.draft = draft_from_status(&status);
            self.dirty = false;
        }
        if self.selected >= self.draft.len() {
            self.selected = self.draft.len().saturating_sub(1);
        }
        if active {
            if !was_preview {
                self.had_preview = true;
                self.preview_layout = Some(if self.dirty {
                    draft_from_status(&status)
                } else {
                    self.draft.clone()
                });
                if let Some(preview) = &status.preview {
                    self.message = format!(
                        "Previewing {}. Keep within {} s or the previous layout returns.",
                        preview.name, preview.seconds_left
                    );
                    self.message_is_error = false;
                }
            }
        } else if was_preview {
            self.had_preview = false;
            self.preview_layout = None;
            self.message = if self.dirty {
                "Preview ended; the previous layout is back. Your unsaved draft remains.".into()
            } else {
                "Preview ended; the previous layout is back.".into()
            };
            self.message_is_error = false;
        }
        self.status = status;
        self.sync_preview_clock(Instant::now());
        self.clamp_scrolls();
    }

    /// Take new profile/startup metadata without replacing an edited draft.
    fn replace_status(&mut self, status: backend::Status) {
        self.status = status;
        self.sync_preview_clock(Instant::now());
        self.clamp_scrolls();
        if self.selected >= self.draft.len() {
            self.selected = self.draft.len().saturating_sub(1);
        }
    }

    fn clamp_scrolls(&mut self) {
        self.profile_scroll = self
            .profile_scroll
            .min(self.status.profiles.len().saturating_sub(1));
    }

    fn checkpoint(&mut self) {
        self.undo.push(self.draft.clone());
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
    }

    pub fn undo(&mut self) {
        match self.undo.pop() {
            Some(draft) => {
                self.draft = draft;
                if self.selected >= self.draft.len() {
                    self.selected = self.draft.len().saturating_sub(1);
                }
                self.dirty = true;
                self.set_message("Undid the last display change.", false);
            }
            None => self.set_message("Nothing to undo.", false),
        }
    }

    pub fn mirror_group(&self, index: usize) -> Vec<usize> {
        let Some(source) = self.draft.get(index) else {
            return Vec::new();
        };
        if !source.enabled {
            return vec![index];
        }
        self.draft
            .iter()
            .enumerate()
            .filter(|(_, output)| output.mirrors(source))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn move_selected(&mut self, dx: i32, dy: i32) {
        let group = self.mirror_group(self.selected);
        if group.is_empty() {
            return;
        }
        self.checkpoint();
        for index in group {
            if let Some(output) = self.draft.get_mut(index) {
                output.x = output.x.saturating_add(dx);
                output.y = output.y.saturating_add(dy);
            }
        }
        self.dirty = true;
    }

    pub fn toggle_enabled(&mut self) {
        let Some(mut next) = self.current().cloned() else {
            return;
        };
        if next.enabled && self.draft.iter().filter(|output| output.enabled).count() == 1 {
            self.set_message("At least one screen must stay enabled.", true);
            return;
        }
        let was_primary = next.primary;
        next.enabled = !next.enabled;
        if next.enabled {
            let right = self
                .draft
                .iter()
                .filter(|output| output.enabled && output.name != next.name)
                .map(|output| output.x + output.width())
                .max()
                .unwrap_or(0);
            let overlaps = self.draft.iter().any(|output| {
                output.enabled && output.name != next.name && outputs_overlap(output, &next)
            });
            if overlaps || (next.x == 0 && next.y == 0) {
                next.x = right;
                next.y = 0;
            }
            if !self
                .draft
                .iter()
                .any(|output| output.enabled && output.primary && output.name != next.name)
            {
                next.primary = true;
            }
        } else {
            next.primary = false;
        }
        self.checkpoint();
        let name = next.name.clone();
        self.draft[self.selected] = next;
        if was_primary {
            if let Some(index) = self
                .draft
                .iter()
                .position(|output| output.enabled && output.name != name)
            {
                self.draft[index].primary = true;
            }
        }
        self.dirty = true;
    }

    pub fn set_primary(&mut self) {
        let Some(selected) = self.current().map(|output| output.name.clone()) else {
            return;
        };
        if !self.current().map(|output| output.enabled).unwrap_or(false) {
            self.set_message("Enable the screen before making it primary.", true);
            return;
        }
        self.checkpoint();
        for output in &mut self.draft {
            output.primary = output.name == selected && output.enabled;
        }
        self.dirty = true;
    }

    /// Step through normal, left, inverted, right.
    pub fn cycle_rotation(&mut self, delta: i32) {
        let Some(current) = self.current().map(|output| output.rotation.clone()) else {
            return;
        };
        let index = ROTATIONS
            .iter()
            .position(|rotation| *rotation == current)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ROTATIONS.len() as i32) as usize;
        self.set_rotation(ROTATIONS[next]);
    }

    /// Rotate the selected screen (and its mirror group) about its centre.
    pub fn set_rotation(&mut self, rotation: &str) {
        let group = self.mirror_group(self.selected);
        let Some(source) = self.current().cloned() else {
            return;
        };
        if !source.enabled {
            self.set_message("Enable the screen before rotating it.", true);
            return;
        }
        if source.rotation == rotation {
            return;
        }
        let rotation = rotation.to_string();
        let (old_width, old_height) = source.size();
        let center_x = source.x + old_width / 2;
        let center_y = source.y + old_height / 2;
        let mut probe = source.clone();
        probe.rotation = rotation.clone();
        let (new_width, new_height) = probe.size();
        let x = center_x - new_width / 2;
        let y = center_y - new_height / 2;
        self.checkpoint();
        for index in group {
            if let Some(output) = self.draft.get_mut(index) {
                output.rotation = rotation.clone();
                output.x = x;
                output.y = y;
            }
        }
        normalize_draft(&mut self.draft);
        self.dirty = true;
    }

    /// The modes the selected screen (or every screen of its mirror group)
    /// supports.
    fn modes_for_selected(&self) -> Vec<backend::DisplayMode> {
        let Some(output) = self.current() else {
            return Vec::new();
        };
        let group = self.mirror_group(self.selected);
        if group.len() > 1 {
            self.common_modes_for_group(&group)
        } else {
            self.status
                .output(&output.name)
                .map(|screen| screen.modes.clone())
                .unwrap_or_default()
        }
    }

    /// Set the selected screen's (and mirror group's) mode, keeping the
    /// rate when the new mode has it.
    pub fn set_mode(&mut self, mode: &backend::DisplayMode) {
        let Some(output) = self.current().cloned() else {
            return;
        };
        let rate = output
            .rate
            .filter(|rate| {
                mode.rates
                    .iter()
                    .any(|candidate| (candidate - rate).abs() < 0.01)
            })
            .or_else(|| mode.rates.first().copied());
        if output.mode == mode.mode && output.rate == rate {
            return;
        }
        self.checkpoint();
        for index in self.mirror_group(self.selected) {
            if let Some(output) = self.draft.get_mut(index) {
                output.mode = mode.mode.clone();
                output.rate = rate;
            }
        }
        self.dirty = true;
    }

    pub fn set_rate(&mut self, rate: f64) {
        if self.current().and_then(|output| output.rate) == Some(rate) {
            return;
        }
        self.checkpoint();
        for index in self.mirror_group(self.selected) {
            if let Some(output) = self.draft.get_mut(index) {
                output.rate = Some(rate);
            }
        }
        self.dirty = true;
    }

    /// U1: the entries of a mode, rate or rotation list for the selected
    /// screen. Modes are sorted by size with the preferred one marked;
    /// rates are those of the chosen mode; mirror groups only list what
    /// every member supports.
    pub fn popover_items(&self, kind: PopKind) -> Result<Vec<PopItem>, String> {
        let output = self.current().ok_or("Select a screen first.")?;
        if !output.enabled {
            return Err(match kind {
                PopKind::Mode => "Enable the screen before choosing a mode.",
                PopKind::Rate => "Enable the screen before choosing a refresh rate.",
                PopKind::Rotation => "Enable the screen before rotating it.",
            }
            .into());
        }
        let items: Vec<PopItem> = match kind {
            PopKind::Mode => {
                let mut modes = self.modes_for_selected();
                let mut seen = Vec::new();
                modes.retain(|mode| {
                    let new = !seen.contains(&mode.mode);
                    seen.push(mode.mode.clone());
                    new
                });
                modes.sort_by_key(|mode| {
                    let (w, h) = mode_size(&mode.mode).unwrap_or((0, 0));
                    (
                        std::cmp::Reverse(i64::from(w) * i64::from(h)),
                        std::cmp::Reverse(w),
                    )
                });
                modes
                    .into_iter()
                    .map(|mode| PopItem {
                        label: mode.mode.clone(),
                        note: mode.preferred.then_some("preferred"),
                        checked: mode.mode == output.mode,
                        value: PopValue::Mode(mode),
                    })
                    .collect()
            }
            PopKind::Rate => {
                let group = self.mirror_group(self.selected);
                let mut rates = if group.len() > 1 {
                    self.common_rates_for_group(&group, &output.mode)
                } else {
                    self.status
                        .output(&output.name)
                        .and_then(|state| state.modes.iter().find(|mode| mode.mode == output.mode))
                        .map(|mode| mode.rates.clone())
                        .unwrap_or_default()
                };
                rates.sort_by(|a, b| b.total_cmp(a));
                rates.dedup_by(|a, b| (*a - *b).abs() < 0.01);
                rates
                    .into_iter()
                    .map(|rate| PopItem {
                        label: format!("{rate:.2} Hz"),
                        note: None,
                        checked: output
                            .rate
                            .is_some_and(|current| (current - rate).abs() < 0.01),
                        value: PopValue::Rate(rate),
                    })
                    .collect()
            }
            PopKind::Rotation => ROTATIONS
                .iter()
                .map(|rotation| PopItem {
                    label: (*rotation).to_string(),
                    note: None,
                    checked: output.rotation == *rotation,
                    value: PopValue::Rotation(rotation),
                })
                .collect(),
        };
        if items.is_empty() {
            return Err(match kind {
                PopKind::Rate => {
                    "No refresh rates are available on every screen in this mirror group."
                }
                _ => "No modes are available on every screen in this mirror group.",
            }
            .into());
        }
        Ok(items)
    }

    pub fn open_popover(&mut self, kind: PopKind) -> Result<(), String> {
        let items = self.popover_items(kind)?;
        let cursor = items.iter().position(|item| item.checked).unwrap_or(0);
        let mut popover = Popover {
            kind,
            items,
            cursor,
            scroll: 0,
        };
        popover.reveal();
        self.focus = Some(match kind {
            PopKind::Mode => Hit::Mode,
            PopKind::Rate => Hit::Rate,
            PopKind::Rotation => Hit::Rotate,
        });
        self.popover = Some(popover);
        Ok(())
    }

    /// Keys while a list is open: arrows move, Enter/Space choose, Esc and
    /// Tab close. Returns false for keys the list does not use.
    pub fn popover_key(&mut self, key: u32) -> bool {
        let Some(popover) = self.popover.as_mut() else {
            return false;
        };
        let last = popover.items.len().saturating_sub(1);
        match key {
            K_UP => popover.cursor = popover.cursor.saturating_sub(1),
            K_DOWN => popover.cursor = (popover.cursor + 1).min(last),
            K_PAGEUP => popover.cursor = popover.cursor.saturating_sub(POP_ROWS),
            K_PAGEDOWN => popover.cursor = (popover.cursor + POP_ROWS).min(last),
            K_HOME => popover.cursor = 0,
            K_END => popover.cursor = last,
            K_RETURN | K_SPACE => {
                let index = popover.cursor;
                self.choose_popover(index);
                return true;
            }
            K_ESCAPE | K_TAB => {
                self.popover = None;
                return true;
            }
            _ => return false,
        }
        popover.reveal();
        true
    }

    /// Apply entry `index` of the open list and close it.
    pub fn choose_popover(&mut self, index: usize) {
        let Some(popover) = self.popover.take() else {
            return;
        };
        let Some(item) = popover.items.into_iter().nth(index) else {
            return;
        };
        match item.value {
            PopValue::Mode(mode) => self.set_mode(&mode),
            PopValue::Rate(rate) => self.set_rate(rate),
            PopValue::Rotation(rotation) => self.set_rotation(rotation),
        }
    }

    fn common_modes_for_group(&self, group: &[usize]) -> Vec<backend::DisplayMode> {
        let mut states = group
            .iter()
            .filter_map(|index| self.draft.get(*index))
            .filter_map(|output| self.status.output(&output.name));
        let Some(first) = states.next() else {
            return Vec::new();
        };
        let others: Vec<&backend::OutputState> = states.collect();
        first
            .modes
            .iter()
            .filter_map(|mode| {
                let mut rates = mode.rates.clone();
                for other in &others {
                    let candidate = other
                        .modes
                        .iter()
                        .find(|candidate| candidate.mode == mode.mode)?;
                    rates.retain(|rate| {
                        candidate
                            .rates
                            .iter()
                            .any(|candidate| (candidate - rate).abs() < 0.01)
                    });
                }
                Some(backend::DisplayMode {
                    mode: mode.mode.clone(),
                    rates,
                    preferred: mode.preferred,
                })
            })
            .collect()
    }

    fn common_rates_for_group(&self, group: &[usize], mode_name: &str) -> Vec<f64> {
        let mut states = group
            .iter()
            .filter_map(|index| self.draft.get(*index))
            .filter_map(|output| self.status.output(&output.name));
        let Some(first) = states.next() else {
            return Vec::new();
        };
        let Some(first_mode) = first.modes.iter().find(|mode| mode.mode == mode_name) else {
            return Vec::new();
        };
        let mut rates = first_mode.rates.clone();
        for other in states {
            let Some(candidate) = other.modes.iter().find(|mode| mode.mode == mode_name) else {
                return Vec::new();
            };
            rates.retain(|rate| {
                candidate
                    .rates
                    .iter()
                    .any(|candidate| (candidate - rate).abs() < 0.01)
            });
        }
        rates
    }

    pub fn mirror_screens(&mut self) {
        let Some(source) = self.current().cloned() else {
            return;
        };
        if !source.enabled {
            self.set_message("Enable the source screen before mirroring.", true);
            return;
        }
        if self.draft.iter().filter(|output| output.enabled).count() < 2 {
            self.set_message("Enable at least two screens to mirror them.", true);
            return;
        }
        let group: Vec<usize> = self
            .draft
            .iter()
            .enumerate()
            .filter(|(_, output)| output.enabled)
            .map(|(index, _)| index)
            .collect();
        let common = self.common_modes_for_group(&group);
        let Some(mode) = common
            .iter()
            .find(|mode| mode.mode == source.mode)
            .or_else(|| common.iter().find(|mode| mode.preferred))
            .or_else(|| common.first())
        else {
            self.set_message(
                "The enabled screens have no common resolution; mirroring was not changed.",
                true,
            );
            return;
        };
        let rate = source
            .rate
            .filter(|rate| {
                mode.rates
                    .iter()
                    .any(|candidate| (candidate - rate).abs() < 0.01)
            })
            .or_else(|| mode.rates.first().copied());
        self.checkpoint();
        for output in &mut self.draft {
            if !output.enabled {
                continue;
            }
            output.mode = mode.mode.clone();
            output.rate = rate;
            output.rotation = source.rotation.clone();
            output.x = source.x;
            output.y = source.y;
            output.primary = output.name == source.name;
        }
        self.dirty = true;
        self.set_message(
            "Mirroring the selected screen on every enabled output. Preview to confirm.",
            false,
        );
    }

    pub fn single_screen(&mut self) {
        let Some(source) = self.current().cloned() else {
            return;
        };
        self.checkpoint();
        for output in &mut self.draft {
            let chosen = output.name == source.name;
            output.enabled = chosen;
            output.primary = chosen;
            if chosen {
                output.x = 0;
                output.y = 0;
            }
        }
        self.dirty = true;
        self.set_message("Only the selected screen stays enabled.", false);
    }

    pub fn extend_row(&mut self) {
        if self.draft.len() < 2 {
            self.set_message("Connect at least two screens to extend the desktop.", true);
            return;
        }
        self.checkpoint();
        let mut cursor = 0;
        for output in &mut self.draft {
            output.enabled = true;
            output.x = cursor;
            output.y = 0;
            cursor = cursor.saturating_add(output.width());
        }
        let primary = self.selected.min(self.draft.len().saturating_sub(1));
        for (index, output) in self.draft.iter_mut().enumerate() {
            output.primary = index == primary;
        }
        self.dirty = true;
        self.set_message("The screens are extended side by side.", false);
    }

    pub fn cycle_mode(&mut self, delta: i32) {
        let Some(output) = self.current().cloned() else {
            return;
        };
        if !output.enabled {
            self.set_message("Enable the screen before choosing a mode.", true);
            return;
        }
        let group = self.mirror_group(self.selected);
        let modes = if group.len() > 1 {
            self.common_modes_for_group(&group)
        } else {
            self.status
                .output(&output.name)
                .map(|screen| screen.modes.clone())
                .unwrap_or_default()
        };
        if modes.is_empty() {
            self.set_message(
                "No modes are available on every screen in this mirror group.",
                true,
            );
            return;
        }
        let index = modes
            .iter()
            .position(|mode| mode.mode == output.mode)
            .unwrap_or(0);
        let next = (index as i32 + delta).rem_euclid(modes.len() as i32) as usize;
        let mode = &modes[next];
        let rate = output
            .rate
            .filter(|rate| {
                mode.rates
                    .iter()
                    .any(|candidate| (candidate - rate).abs() < 0.01)
            })
            .or_else(|| mode.rates.first().copied());
        let (mode, rate) = (mode.mode.clone(), rate);
        self.checkpoint();
        for index in group {
            if let Some(output) = self.draft.get_mut(index) {
                output.mode = mode.clone();
                output.rate = rate;
            }
        }
        self.dirty = true;
    }

    pub fn cycle_rate(&mut self, delta: i32) {
        let Some(output) = self.current().cloned() else {
            return;
        };
        if !output.enabled {
            self.set_message("Enable the screen before choosing a refresh rate.", true);
            return;
        }
        let group = self.mirror_group(self.selected);
        let rates = if group.len() > 1 {
            self.common_rates_for_group(&group, &output.mode)
        } else {
            self.status
                .output(&output.name)
                .and_then(|state| {
                    state
                        .modes
                        .iter()
                        .find(|mode| mode.mode == output.mode)
                        .map(|mode| mode.rates.clone())
                })
                .unwrap_or_default()
        };
        if rates.is_empty() {
            self.set_message(
                "No refresh rates are available on every screen in this mirror group.",
                true,
            );
            return;
        }
        let index = output
            .rate
            .and_then(|rate| {
                rates
                    .iter()
                    .position(|candidate| (candidate - rate).abs() < 0.01)
            })
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(rates.len() as i32) as usize;
        let Some(rate) = rates.get(next).copied() else {
            return;
        };
        self.checkpoint();
        for index in group {
            if let Some(output) = self.draft.get_mut(index) {
                output.rate = Some(rate);
            }
        }
        self.dirty = true;
    }

    pub fn profile_from_draft(&self) -> backend::Profile {
        let mut normalized = normalized_draft(&self.draft);
        single_primary(&mut normalized);
        backend::Profile {
            name: self
                .selected_profile
                .and_then(|index| self.status.profiles.get(index))
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "Draft".into()),
            outputs: normalized
                .iter()
                .filter(|output| output.enabled && output.connected)
                .map(|output| backend::ProfileOutput {
                    name: output.name.clone(),
                    mode: output.mode.clone(),
                    rate: output.rate,
                    position: backend::position_string(output.x, output.y),
                    rotation: output.rotation.clone(),
                    primary: output.primary,
                    label: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// D10/U3: a click on a profile row selects it, and loads it into the
    /// draft when every screen it needs is connected. A profile that cannot
    /// be used here stays selectable for Rename, Delete and Use at login.
    /// Returns true when the profile was loaded.
    pub fn select_profile(&mut self, index: usize) -> bool {
        let Some(profile) = self.status.profiles.get(index) else {
            return false;
        };
        let missing = missing_outputs(&self.status, profile);
        if missing.is_empty() {
            match self.load_profile(index) {
                Ok(()) => return true,
                Err(err) => self.set_message(err, true),
            }
            return false;
        }
        let name = profile.name.clone();
        self.selected_profile = Some(index);
        self.profile_scroll = index;
        self.set_message(
            format!(
                "{name} needs {}. Rename, Delete and Use at login still work.",
                missing.join(", ")
            ),
            false,
        );
        false
    }

    pub fn load_profile(&mut self, index: usize) -> Result<(), String> {
        let profile = self
            .status
            .profiles
            .get(index)
            .cloned()
            .ok_or("unknown display profile")?;
        let missing = missing_outputs(&self.status, &profile);
        if !missing.is_empty() {
            return Err(format!(
                "{} is not connected; connect {} and try again",
                missing.join(", "),
                if missing.len() == 1 { "it" } else { "them" }
            ));
        }
        self.checkpoint();
        let mut draft = self.draft.clone();
        for output in &mut draft {
            output.enabled = false;
            output.primary = false;
        }
        for saved in &profile.outputs {
            if let Some(output) = draft.iter_mut().find(|output| output.name == saved.name) {
                output.enabled = true;
                output.mode = saved.mode.clone();
                output.rate = saved.rate;
                output.rotation = saved.rotation.clone();
                output.primary = saved.primary;
                if let Some((x, y)) = saved.position_pair() {
                    output.x = x;
                    output.y = y;
                }
            }
        }
        normalize_draft(&mut draft);
        single_primary(&mut draft);
        self.draft = draft;
        self.selected = self
            .draft
            .iter()
            .position(|output| output.enabled)
            .unwrap_or(0);
        self.selected_profile = Some(index);
        self.output_scroll = 0;
        self.profile_scroll = index;
        self.dirty = true;
        self.set_message(
            format!("Loaded {}. Apply to preview it.", profile.name),
            false,
        );
        Ok(())
    }

    /// The name of the profile that was loaded into the draft, if any.
    pub fn loaded_profile_name(&self) -> Option<&str> {
        self.selected_profile
            .and_then(|index| self.status.profiles.get(index))
            .map(|profile| profile.name.as_str())
    }

    /// D3: saving under another existing profile's name replaces it and
    /// asks first. Saving under the loaded profile's own name is an update.
    pub fn save_needs_confirm(&self, name: &str) -> bool {
        let name = name.trim();
        !name.is_empty()
            && self.loaded_profile_name() != Some(name)
            && self
                .status
                .profiles
                .iter()
                .any(|profile| profile.name == name)
    }

    pub fn apply_preview(&mut self) -> Result<(), String> {
        let issues = validate_draft(&self.draft);
        if let Some(issue) = issues.iter().find(|issue| issue.blocking) {
            return Err(issue.text.clone());
        }
        if normalize_draft(&mut self.draft) {
            self.dirty = true;
        }
        let profile = self.profile_from_draft();
        let layout = self.draft.clone();
        self.submit(
            backend::Job::Preview {
                profile,
                seconds: PREVIEW_SECONDS,
            },
            After::Preview { layout },
        );
        Ok(())
    }

    pub fn keep(&mut self) -> Result<(), String> {
        if self.preview_changed() {
            return Err(
                "The draft changed after the preview; press Preview again to apply it.".into(),
            );
        }
        self.submit(backend::Job::Keep, After::Keep { quit: false });
        Ok(())
    }

    pub fn revert(&mut self) -> Result<(), String> {
        self.submit(backend::Job::Revert, After::Revert { quit: false });
        Ok(())
    }

    pub fn save(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a profile name is required".into());
        }
        let issues = validate_draft(&self.draft);
        if let Some(issue) = issues.iter().find(|issue| issue.blocking) {
            return Err(issue.text.clone());
        }
        if normalize_draft(&mut self.draft) {
            self.dirty = true;
        }
        let mut profile = self.profile_from_draft();
        profile.name = name.to_string();
        let update = self.status.profiles.iter().any(|p| p.name == name);
        self.submit(
            backend::Job::Save {
                profile,
                name: name.to_string(),
            },
            After::Save {
                name: name.to_string(),
                update,
            },
        );
        Ok(())
    }

    pub fn rename(&mut self, new: &str) -> Result<(), String> {
        let Some(index) = self.selected_profile else {
            return Err("select a saved profile first".into());
        };
        let old = self
            .status
            .profiles
            .get(index)
            .map(|profile| profile.name.clone())
            .ok_or("unknown display profile")?;
        let new = new.trim().to_string();
        self.submit(
            backend::Job::Rename {
                old: old.clone(),
                new: new.clone(),
            },
            After::Rename { old, new },
        );
        Ok(())
    }

    pub fn delete_named(&mut self, name: &str) -> Result<(), String> {
        self.submit(
            backend::Job::Delete {
                name: name.to_string(),
            },
            After::Delete {
                name: name.to_string(),
            },
        );
        Ok(())
    }

    /// Whether niructl offers automatic switching (0.17 and later).
    pub fn automatic_supported(&self) -> bool {
        self.status.automatic.is_some()
    }

    pub fn automatic_enabled(&self) -> bool {
        self.status
            .automatic
            .as_ref()
            .is_some_and(|auto| auto.enabled)
    }

    /// The screens show a layout the automation made, not a saved profile:
    /// what the header offers to save.
    pub fn automatic_layout(&self) -> Option<&str> {
        let auto = self.status.automatic.as_ref()?;
        if self.dirty || auto.current_profile.is_some() {
            return None;
        }
        auto.automatic_layout.as_deref()
    }

    pub fn toggle_automatic(&mut self) -> Result<(), String> {
        if !self.automatic_supported() {
            return Err("This niructl has no automatic switching; update NIRUX.".into());
        }
        let enabled = !self.automatic_enabled();
        self.submit(
            backend::Job::Automatic { enabled },
            After::Automatic { enabled },
        );
        Ok(())
    }

    /// Toggle "Use at login" for the selected profile.
    pub fn toggle_startup(&mut self) -> Result<(), String> {
        let name = self
            .selected_profile
            .and_then(|index| self.status.profiles.get(index))
            .map(|profile| profile.name.clone())
            .ok_or("Select a saved profile first.")?;
        let next = (self.status.startup_profile.as_deref() != Some(name.as_str())).then_some(name);
        let enabled = next.is_some();
        self.submit(
            backend::Job::Startup { name: next },
            After::Startup { enabled },
        );
        Ok(())
    }
}

/// D10: the outputs a profile needs that are not connected and usable.
/// niructl 0.17+ matches screens by identity and says whether the profile can
/// be loaded; its answer wins over comparing connector names.
pub fn missing_outputs(status: &backend::Status, profile: &backend::Profile) -> Vec<String> {
    if profile.loadable == Some(false) {
        return vec![profile
            .reason
            .clone()
            .map(|reason| reason.trim_start_matches("needs ").to_string())
            .unwrap_or_else(|| "a screen that is not connected".into())];
    }
    if profile.loadable == Some(true) {
        return Vec::new();
    }
    profile
        .outputs
        .iter()
        .filter(|output| {
            status
                .output(&output.name)
                .map(|state| !state.usable())
                .unwrap_or(true)
        })
        .map(|output| output.name.clone())
        .collect()
}

/// D10: true when the profile describes the running layout: the same
/// enabled outputs with the same mode, position, rotation and primary, and
/// the same rate where the profile names one.
pub fn profile_is_current(profile: &backend::Profile, actual: &[DraftOutput]) -> bool {
    let enabled: Vec<&DraftOutput> = actual.iter().filter(|output| output.enabled).collect();
    if profile.outputs.is_empty() || profile.outputs.len() != enabled.len() {
        return false;
    }
    let single = enabled.len() == 1;
    profile.outputs.iter().all(|saved| {
        let Some(output) = enabled.iter().find(|output| output.name == saved.name) else {
            return false;
        };
        let rotation = if saved.rotation.is_empty() {
            "normal"
        } else {
            saved.rotation.as_str()
        };
        saved.mode == output.mode
            && rotation == output.rotation
            && saved.position_pair() == Some((output.x, output.y))
            && (single || saved.primary == output.primary)
            && match (saved.rate, output.rate) {
                (Some(left), Some(right)) => (left - right).abs() < 0.01,
                (Some(_), None) => false,
                (None, _) => true,
            }
    })
}

/// D10: "3 screens · eDP + 2× 2560x1440". A built-in panel is named by its
/// connector type; external screens by their mode.
pub fn profile_summary(profile: &backend::Profile) -> String {
    let mut parts: Vec<(String, usize)> = Vec::new();
    for output in &profile.outputs {
        let kind = output.name.split('-').next().unwrap_or_default();
        let label = if ["eDP", "LVDS", "DSI"]
            .iter()
            .any(|internal| kind.eq_ignore_ascii_case(internal))
        {
            kind.to_string()
        } else if output.mode.is_empty() {
            output.name.clone()
        } else {
            output.mode.clone()
        };
        match parts.iter_mut().find(|(candidate, _)| *candidate == label) {
            Some((_, count)) => *count += 1,
            None => parts.push((label, 1)),
        }
    }
    let count = profile.outputs.len();
    let list = parts
        .into_iter()
        .map(|(label, count)| {
            if count > 1 {
                format!("{count}\u{d7} {label}")
            } else {
                label
            }
        })
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "{count} screen{} · {list}",
        if count == 1 { "" } else { "s" }
    )
}

/// What a profile row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub missing: Vec<String>,
    pub current: bool,
    pub startup: bool,
    pub summary: String,
    /// niructl's explanation when the profile does not fit, e.g. "needs HPN
    /// HP E27q G5 (serial …2MRJ)" or "also connected: HDMI-1".
    pub reason: Option<String>,
    /// The automation applied this profile and it is what the screens show.
    pub auto_applied: bool,
    /// `auto = false`: never chosen automatically.
    pub manual_only: bool,
}

pub fn profile_info(status: &backend::Status, profile: &backend::Profile) -> ProfileInfo {
    let current = profile_is_current(profile, &draft_from_status(status));
    // niructl matches by screen identity, so its verdict wins; an older
    // niructl only lets us compare connector names.
    let missing = match profile.fit.as_deref() {
        // The reason names the screens by identity ("needs HPN HP E27q G5
        // (serial …2MRJ)", "the built-in screen (the lid is closed)").
        Some("missing") => vec![profile
            .reason
            .clone()
            .unwrap_or_else(|| "a screen that is not connected".into())],
        Some(_) => Vec::new(),
        None => missing_outputs(status, profile),
    };
    let auto_applied = status.automatic.as_ref().is_some_and(|auto| {
        auto.enabled
            && current
            && auto.last.as_ref().is_some_and(|last| {
                last.action == "profile" && last.profile.as_deref() == Some(profile.name.as_str())
            })
    });
    ProfileInfo {
        missing,
        current,
        startup: status.startup_profile.as_deref() == Some(profile.name.as_str()),
        summary: profile_summary(profile),
        reason: profile.reason.clone(),
        auto_applied,
        manual_only: !profile.auto,
    }
}

fn draft_from_status(status: &backend::Status) -> Vec<DraftOutput> {
    let mut draft = Vec::new();
    let mut next_x = 0;
    for state in status.outputs.iter().filter(|output| output.connected) {
        if !state.usable() {
            continue;
        }
        let mode = state
            .current
            .as_ref()
            .map(|current| current.mode.clone())
            .or_else(|| {
                state
                    .modes
                    .iter()
                    .find(|mode| mode.preferred)
                    .map(|mode| mode.mode.clone())
            })
            .or_else(|| state.modes.first().map(|mode| mode.mode.clone()))
            .unwrap_or_default();
        let rate = state
            .current
            .as_ref()
            .and_then(|current| current.rate)
            .or_else(|| {
                state
                    .modes
                    .iter()
                    .find(|mode| mode.preferred)
                    .and_then(|mode| mode.rates.first().copied())
            })
            .or_else(|| {
                state
                    .modes
                    .first()
                    .and_then(|mode| mode.rates.first().copied())
            });
        let (x, y) = state
            .current
            .as_ref()
            .and_then(|current| current.position_pair())
            .unwrap_or((next_x, 0));
        let output = DraftOutput {
            name: state.name.clone(),
            connected: state.connected,
            enabled: state.active,
            mode,
            rate,
            x,
            y,
            rotation: state
                .current
                .as_ref()
                .map(|current| current.rotation.clone())
                .filter(|rotation| !rotation.is_empty())
                .unwrap_or_else(|| "normal".into()),
            primary: state.primary,
        };
        if output.enabled {
            next_x = next_x.max(output.x + output.width());
        }
        draft.push(output);
    }
    single_primary(&mut draft);
    draft
}

/// D7: with exactly one enabled screen that screen is the primary one,
/// whatever RandR reports. Many laptops have no RandR primary at all.
fn single_primary(draft: &mut [DraftOutput]) {
    let mut enabled = draft.iter_mut().filter(|output| output.enabled);
    if let (Some(only), None) = (enabled.next(), enabled.next()) {
        only.primary = true;
    }
}

fn normalize_draft(draft: &mut [DraftOutput]) -> bool {
    if draft.is_empty() {
        return false;
    }
    let active: Vec<&DraftOutput> = draft.iter().filter(|output| output.enabled).collect();
    let pool = if active.is_empty() {
        draft.iter().collect::<Vec<_>>()
    } else {
        active
    };
    let min_x = pool.iter().map(|output| output.x).min().unwrap_or(0);
    let min_y = pool.iter().map(|output| output.y).min().unwrap_or(0);
    if min_x == 0 && min_y == 0 {
        return false;
    }
    for output in draft.iter_mut() {
        output.x = output.x.saturating_sub(min_x);
        output.y = output.y.saturating_sub(min_y);
    }
    true
}

fn normalized_draft(draft: &[DraftOutput]) -> Vec<DraftOutput> {
    let mut copy = draft.to_vec();
    normalize_draft(&mut copy);
    copy
}

fn outputs_overlap(left: &DraftOutput, right: &DraftOutput) -> bool {
    let left_right = i64::from(left.x) + i64::from(left.width());
    let left_bottom = i64::from(left.y) + i64::from(left.height());
    let right_right = i64::from(right.x) + i64::from(right.width());
    let right_bottom = i64::from(right.y) + i64::from(right.height());
    i64::from(left.x) < right_right
        && i64::from(right.x) < left_right
        && i64::from(left.y) < right_bottom
        && i64::from(right.y) < left_bottom
}

#[cfg(test)]
fn rects_overlap(left: Rectangle, right: Rectangle) -> bool {
    let left_right = i32::from(left.x) + i32::from(left.width);
    let left_bottom = i32::from(left.y) + i32::from(left.height);
    let right_right = i32::from(right.x) + i32::from(right.width);
    let right_bottom = i32::from(right.y) + i32::from(right.height);
    i32::from(left.x) < right_right
        && i32::from(right.x) < left_right
        && i32::from(left.y) < right_bottom
        && i32::from(right.y) < left_bottom
}

#[derive(Clone, Debug)]
struct Issue {
    blocking: bool,
    text: String,
}

fn validate_draft(draft: &[DraftOutput]) -> Vec<Issue> {
    let mut issues = Vec::new();
    let enabled: Vec<&DraftOutput> = draft.iter().filter(|output| output.enabled).collect();
    if enabled.is_empty() {
        issues.push(Issue {
            blocking: true,
            text: "At least one screen must be enabled.".into(),
        });
        return issues;
    }
    if enabled
        .iter()
        .any(|output| output.mode.is_empty() || mode_size(&output.mode).is_none())
    {
        issues.push(Issue {
            blocking: true,
            text: "A selected screen has no usable display mode.".into(),
        });
    }
    if enabled.len() > 1 && !enabled.iter().any(|output| output.primary) {
        issues.push(Issue {
            blocking: false,
            text: "No enabled screen is set as primary.".into(),
        });
    }
    if enabled.iter().filter(|output| output.primary).count() > 1 {
        issues.push(Issue {
            blocking: true,
            text: "More than one screen is marked primary.".into(),
        });
    }
    for (index, left) in enabled.iter().enumerate() {
        for right in enabled.iter().skip(index + 1) {
            if !left.mirrors(right) && outputs_overlap(left, right) {
                issues.push(Issue {
                    blocking: true,
                    text: format!("{} and {} overlap.", left.name, right.name),
                });
            }
        }
    }
    let max_x = enabled
        .iter()
        .map(|output| output.x.saturating_add(output.width()))
        .max()
        .unwrap_or(0);
    let max_y = enabled
        .iter()
        .map(|output| output.y.saturating_add(output.height()))
        .max()
        .unwrap_or(0);
    if max_x > i32::from(i16::MAX) || max_y > i32::from(i16::MAX) {
        issues.push(Issue {
            blocking: true,
            text: "The desktop would be larger than the X server can address.".into(),
        });
    }
    if enabled.iter().any(|output| output.x < 0 || output.y < 0) {
        issues.push(Issue {
            blocking: false,
            text: "The layout is moved to non-negative coordinates when it is applied.".into(),
        });
    }
    issues
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub canvas: Rectangle,
    pub panel: Rectangle,
    pub profiles: Rectangle,
    pub footer: Rectangle,
    /// D6: in the compact layout the canvas, panel and profiles scroll
    /// inside this rectangle, between the header and the footer.
    pub viewport: Option<Rectangle>,
    /// How far the compact content can scroll.
    pub max_scroll: i16,
    /// Smaller than the compact layout supports.
    pub too_small: bool,
}

fn rect(x: i16, y: i16, width: i32, height: i32) -> Rectangle {
    Rectangle {
        x,
        y,
        width: width.clamp(0, i32::from(u16::MAX)) as u16,
        height: height.clamp(0, i32::from(u16::MAX)) as u16,
    }
}

pub fn layout(w: u16, h: u16) -> Layout {
    layout_scrolled(w, h, 0)
}

/// The window's regions at `w`x`h`, with the compact content scrolled by
/// `scroll` pixels (clamped).
pub fn layout_scrolled(w: u16, h: u16, scroll: i16) -> Layout {
    let (wi, hi) = (i32::from(w), i32::from(h));
    let pad = i32::from(PAD);
    let footer = rect(
        PAD,
        (hi - i32::from(FOOTER)) as i16,
        wi - pad * 2,
        i32::from(FOOTER),
    );
    if w < SMALLEST_WIDTH || h < SMALLEST_HEIGHT {
        let empty = rect(0, 0, 0, 0);
        return Layout {
            canvas: empty,
            panel: empty,
            profiles: empty,
            footer,
            viewport: None,
            max_scroll: 0,
            too_small: true,
        };
    }
    if w >= MIN_WIDTH && h >= MIN_HEIGHT {
        let canvas_height = hi - i32::from(HEADER + FOOTER + PROFILE_H + 16);
        let canvas = rect(
            PAD,
            HEADER,
            wi - i32::from(PAD * 2 + PANEL_W + 12),
            canvas_height,
        );
        let panel = rect(
            (wi - pad - i32::from(PANEL_W)) as i16,
            HEADER,
            i32::from(PANEL_W),
            canvas_height,
        );
        let profiles = rect(
            PAD,
            (i32::from(HEADER) + canvas_height + 10) as i16,
            wi - pad * 2,
            i32::from(PROFILE_H),
        );
        return Layout {
            canvas,
            panel,
            profiles,
            footer,
            viewport: None,
            max_scroll: 0,
            too_small: false,
        };
    }
    let header = i32::from(HEADER_COMPACT);
    let viewport = rect(0, HEADER_COMPACT, wi, hi - header - i32::from(FOOTER) - 4);
    let view_h = i32::from(viewport.height);
    let (top_h, side) = if w >= MIN_WIDTH {
        // Wide but short: canvas and panel side by side, as usual.
        (
            i32::from(PANEL_MIN_H).max(view_h - i32::from(PROFILE_H) - 10),
            true,
        )
    } else {
        ((view_h * 55 / 100).clamp(140, 260), false)
    };
    let content = if side {
        top_h + 10 + i32::from(PROFILE_H) + 6
    } else {
        top_h + 10 + i32::from(PANEL_MIN_H) + 10 + i32::from(PROFILE_H) + 6
    };
    let max_scroll = (content - view_h).clamp(0, i32::from(i16::MAX)) as i16;
    let scroll = i32::from(scroll.clamp(0, max_scroll));
    let top = header - scroll;
    let (canvas, panel, below) = if side {
        (
            rect(
                PAD,
                top as i16,
                wi - i32::from(PAD * 2 + PANEL_W + 12),
                top_h,
            ),
            rect(
                (wi - pad - i32::from(PANEL_W)) as i16,
                top as i16,
                i32::from(PANEL_W),
                top_h,
            ),
            top + top_h + 10,
        )
    } else {
        let panel_y = top + top_h + 10;
        (
            rect(PAD, top as i16, wi - pad * 2, top_h),
            rect(PAD, panel_y as i16, wi - pad * 2, i32::from(PANEL_MIN_H)),
            panel_y + i32::from(PANEL_MIN_H) + 10,
        )
    };
    let profiles = rect(PAD, below as i16, wi - pad * 2, i32::from(PROFILE_H));
    Layout {
        canvas,
        panel,
        profiles,
        footer,
        viewport: Some(viewport),
        max_scroll,
        too_small: false,
    }
}

fn intersect(a: Rectangle, b: Rectangle) -> Rectangle {
    let x0 = i32::from(a.x).max(i32::from(b.x));
    let y0 = i32::from(a.y).max(i32::from(b.y));
    let x1 = (i32::from(a.x) + i32::from(a.width)).min(i32::from(b.x) + i32::from(b.width));
    let y1 = (i32::from(a.y) + i32::from(a.height)).min(i32::from(b.y) + i32::from(b.height));
    rect(x0 as i16, y0 as i16, x1 - x0, y1 - y0)
}

#[derive(Clone, Copy, Debug)]
struct Canvas {
    x: f32,
    y: f32,
    scale: f32,
}

fn draft_bounds(draft: &[DraftOutput]) -> (i32, i32, i32, i32) {
    if draft.is_empty() {
        return (0, 0, 1920, 1080);
    }
    let min_x = draft.iter().map(|output| output.x).min().unwrap_or(0);
    let min_y = draft.iter().map(|output| output.y).min().unwrap_or(0);
    let max_x = draft
        .iter()
        .map(|output| output.x + output.width())
        .max()
        .unwrap_or(1920);
    let max_y = draft
        .iter()
        .map(|output| output.y + output.height())
        .max()
        .unwrap_or(1080);
    (min_x, min_y, (max_x - min_x).max(1), (max_y - min_y).max(1))
}

fn canvas_for(area: Rectangle, draft: &[DraftOutput], zoom: f32, pan: (f32, f32)) -> Canvas {
    let (min_x, min_y, width, height) = draft_bounds(draft);
    let margin = 34.0;
    let avail_w = (f32::from(area.width) - margin * 2.0).max(1.0);
    let avail_h = (f32::from(area.height) - margin * 2.0).max(1.0);
    let fit = (avail_w / width as f32)
        .min(avail_h / height as f32)
        .clamp(0.02, 1.0);
    let scale = (fit * zoom).clamp(0.01, 4.0);
    let drawn_w = width as f32 * scale;
    let drawn_h = height as f32 * scale;
    Canvas {
        x: f32::from(area.x) + (f32::from(area.width) - drawn_w) / 2.0 + pan.0
            - min_x as f32 * scale,
        y: f32::from(area.y) + (f32::from(area.height) - drawn_h) / 2.0 + pan.1
            - min_y as f32 * scale,
        scale,
    }
}

fn monitor_rect(canvas: Canvas, output: &DraftOutput) -> Rectangle {
    Rectangle {
        x: (canvas.x + output.x as f32 * canvas.scale).round() as i16,
        y: (canvas.y + output.y as f32 * canvas.scale).round() as i16,
        width: (output.width() as f32 * canvas.scale).round().max(12.0) as u16,
        height: (output.height() as f32 * canvas.scale).round().max(9.0) as u16,
    }
}

#[derive(Clone, Debug)]
struct MonitorView {
    index: usize,
    members: Vec<usize>,
    rect: Rectangle,
    selected: bool,
    primary: bool,
    enabled: bool,
    overlapping: bool,
}

fn monitor_views(state: &State, canvas: Canvas) -> Vec<MonitorView> {
    let mut views: Vec<MonitorView> = Vec::new();
    for (index, output) in state.draft.iter().enumerate() {
        if let Some(view) = views.iter_mut().find(|view| {
            state
                .draft
                .get(view.members[0])
                .map(|first| first.same_layout(output))
                .unwrap_or(false)
        }) {
            view.members.push(index);
        } else {
            views.push(MonitorView {
                index,
                members: vec![index],
                rect: monitor_rect(canvas, output),
                selected: false,
                primary: false,
                enabled: output.enabled,
                overlapping: false,
            });
        }
    }
    for view in &mut views {
        if view.members.contains(&state.selected) {
            view.index = state.selected;
        } else if let Some(index) = view.members.iter().copied().find(|index| {
            state
                .draft
                .get(*index)
                .map(|output| output.primary)
                .unwrap_or(false)
        }) {
            view.index = index;
        }
        view.selected = view.members.contains(&state.selected);
        view.primary = view.members.iter().any(|index| {
            state
                .draft
                .get(*index)
                .map(|output| output.primary)
                .unwrap_or(false)
        });
        view.enabled = view.members.iter().any(|index| {
            state
                .draft
                .get(*index)
                .map(|output| output.enabled)
                .unwrap_or(false)
        });
    }
    let mut overlaps = Vec::new();
    for (left, view) in views.iter().enumerate() {
        for (right, other) in views.iter().enumerate().skip(left + 1) {
            if !view.enabled || !other.enabled {
                continue;
            }
            let overlaps_draft = view.members.iter().any(|left_index| {
                other.members.iter().any(|right_index| {
                    let left = &state.draft[*left_index];
                    let right = &state.draft[*right_index];
                    !left.mirrors(right) && outputs_overlap(left, right)
                })
            });
            if overlaps_draft {
                overlaps.push((left, right));
            }
        }
    }
    for (left, right) in overlaps {
        views[left].overlapping = true;
        views[right].overlapping = true;
    }
    views
}

#[derive(Clone, Debug)]
struct PanelLayout {
    rows: Vec<(usize, Rectangle)>,
    page_prev: Option<Rectangle>,
    page_next: Option<Rectangle>,
    primary: Rectangle,
    enabled: Rectangle,
    rotate: Rectangle,
    identify: Rectangle,
    mode: Rectangle,
    rate: Rectangle,
    single: Rectangle,
    extend: Rectangle,
    mirror: Rectangle,
    info: Rectangle,
    apply: [Rectangle; 3],
    visible: usize,
    scroll: usize,
    total: usize,
}

fn panel_layout(state: &State, l: Layout) -> PanelLayout {
    let x = l.panel.x + 10;
    let width = l.panel.width.saturating_sub(20);
    let apply_y = l.panel.y + i16::try_from(l.panel.height).unwrap_or(0) - 38;
    let info_y = apply_y - 52;
    let props_y = info_y - 128;
    let list_top = l.panel.y + 28;
    let list_bottom = props_y - 8;
    let available = (list_bottom - list_top).max(ROW_H);
    let max_rows = ((available / ROW_H) as usize).clamp(1, 6);
    let total = state.draft.len();
    let visible = total.min(max_rows);
    let scroll = state.output_scroll.min(total.saturating_sub(visible));
    let rows = (0..visible)
        .map(|slot| {
            let index = scroll + slot;
            (
                index,
                Rectangle {
                    x,
                    y: list_top + slot as i16 * ROW_H,
                    width,
                    height: ROW_H as u16 - 2,
                },
            )
        })
        .collect();
    let (page_prev, page_next) = if total > visible {
        (
            Some(Rectangle {
                x: l.panel.x + i16::try_from(l.panel.width).unwrap_or(0) - 54,
                y: l.panel.y + 4,
                width: 22,
                height: 20,
            }),
            Some(Rectangle {
                x: l.panel.x + i16::try_from(l.panel.width).unwrap_or(0) - 28,
                y: l.panel.y + 4,
                width: 22,
                height: 20,
            }),
        )
    } else {
        (None, None)
    };
    let gap = 6u16;
    let gap_i = 6i16;
    let half = (width.saturating_sub(gap)) / 2;
    let third = (width.saturating_sub(gap * 2)) / 3;
    let pair = |row: i16, right: bool, w: u16| Rectangle {
        x: x + if right {
            i16::try_from(w).unwrap_or(0) + gap_i
        } else {
            0
        },
        y: props_y + row * 30,
        width: w,
        height: 28,
    };
    let third_rect = |slot: i16| Rectangle {
        x: x + slot * (i16::try_from(third).unwrap_or(0) + gap_i),
        y: props_y + 90,
        width: third,
        height: 28,
    };
    let apply_width = l.panel.width.saturating_sub(32) / 3;
    let apply = [
        Rectangle {
            x,
            y: apply_y,
            width: apply_width,
            height: 28,
        },
        Rectangle {
            x: x + i16::try_from(apply_width).unwrap_or(0) + gap_i,
            y: apply_y,
            width: apply_width,
            height: 28,
        },
        Rectangle {
            x: x + i16::try_from(apply_width).unwrap_or(0) * 2 + gap_i * 2,
            y: apply_y,
            width: apply_width,
            height: 28,
        },
    ];
    PanelLayout {
        rows,
        page_prev,
        page_next,
        primary: pair(0, false, half),
        enabled: pair(0, true, half),
        rotate: pair(1, false, half),
        identify: pair(1, true, half),
        mode: pair(2, false, half),
        rate: pair(2, true, half),
        single: third_rect(0),
        extend: third_rect(1),
        mirror: third_rect(2),
        info: Rectangle {
            x,
            y: info_y,
            width,
            height: 44,
        },
        apply,
        visible,
        scroll,
        total,
    }
}

#[derive(Clone, Debug)]
struct ProfileLayout {
    rows: Vec<(usize, Rectangle)>,
    prev: Option<Rectangle>,
    next: Option<Rectangle>,
    /// The automatic-switching toggle in the header; None with an older
    /// niructl that has no automation.
    automatic: Option<Rectangle>,
    actions: [Rectangle; 5],
    page: usize,
    pages: usize,
    per_page: usize,
}

/// Width of the "Automatic: on/off" toggle.
const AUTOMATIC_W: u16 = 116;

fn profile_layout(state: &State, l: Layout) -> ProfileLayout {
    let total = state.status.profiles.len();
    let actions_y = l.profiles.y + i16::try_from(l.profiles.height).unwrap_or(0) - 33;
    let top = l.profiles.y + 28;
    let step = PROFILE_ROW_H + PROFILE_ROW_GAP;
    let per_column = (((actions_y - 4 - top + PROFILE_ROW_GAP) / step).max(1)) as usize;
    let columns = if l.profiles.width >= 700 { 2usize } else { 1 };
    let per_page = per_column * columns;
    let pages = total.div_ceil(per_page).max(1);
    let page = (state.profile_scroll / per_page).min(pages.saturating_sub(1));
    let start = page * per_page;
    let visible = (total.saturating_sub(start)).min(per_page);
    let inner = l.profiles.width.saturating_sub(16);
    let gap = 8u16;
    let width = (inner.saturating_sub(gap * (columns as u16 - 1))) / columns as u16;
    let rows = (0..visible)
        .map(|slot| {
            // Fill a column top to bottom, then the next one.
            let column = slot / per_column;
            let row = slot % per_column;
            (
                start + slot,
                Rectangle {
                    x: l.profiles.x + 8 + column as i16 * i16::try_from(width + gap).unwrap_or(0),
                    y: top + row as i16 * step,
                    width,
                    height: PROFILE_ROW_H as u16,
                },
            )
        })
        .collect();
    let (prev, next) = if total > per_page {
        (
            Some(Rectangle {
                x: l.profiles.x + i16::try_from(l.profiles.width).unwrap_or(0) - 52,
                y: l.profiles.y + 2,
                width: 22,
                height: 20,
            }),
            Some(Rectangle {
                x: l.profiles.x + i16::try_from(l.profiles.width).unwrap_or(0) - 26,
                y: l.profiles.y + 2,
                width: 22,
                height: 20,
            }),
        )
    } else {
        (None, None)
    };
    let automatic = state.automatic_supported().then(|| {
        let right = l.profiles.x + i16::try_from(l.profiles.width).unwrap_or(0)
            - if prev.is_some() { 58 } else { 4 };
        Rectangle {
            x: right - AUTOMATIC_W as i16,
            y: l.profiles.y + 2,
            width: AUTOMATIC_W,
            height: 20,
        }
    });
    let action_width = (l.profiles.width.saturating_sub(24)) / 5;
    let actions = std::array::from_fn(|index| Rectangle {
        x: l.profiles.x + index as i16 * i16::try_from(action_width + 4).unwrap_or(0),
        y: actions_y,
        width: action_width,
        height: 27,
    });
    ProfileLayout {
        rows,
        prev,
        next,
        automatic,
        actions,
        page,
        pages,
        per_page,
    }
}

#[derive(Clone, Debug)]
struct Ui {
    l: Layout,
    canvas: Canvas,
    monitors: Vec<MonitorView>,
    panel: PanelLayout,
    profiles: ProfileLayout,
}

impl Ui {
    #[cfg(test)]
    fn new(
        state: &State,
        width: u16,
        height: u16,
        frozen: Option<Canvas>,
        zoom: f32,
        pan: (f32, f32),
    ) -> Self {
        Self::scrolled(state, width, height, frozen, zoom, pan, 0)
    }

    fn scrolled(
        state: &State,
        width: u16,
        height: u16,
        frozen: Option<Canvas>,
        zoom: f32,
        pan: (f32, f32),
        scroll: i16,
    ) -> Self {
        let l = layout_scrolled(width, height, scroll);
        let canvas = frozen.unwrap_or_else(|| canvas_for(l.canvas, &state.draft, zoom, pan));
        let monitors = monitor_views(state, canvas);
        let panel = panel_layout(state, l);
        let profiles = profile_layout(state, l);
        Self {
            l,
            canvas,
            monitors,
            panel,
            profiles,
        }
    }
}

fn name_rect(w: u16, h: u16) -> Rectangle {
    let width = 560.min(w.saturating_sub(80));
    let height = 150u16;
    Rectangle {
        x: (i16::try_from(w).unwrap_or(i16::MAX) - i16::try_from(width).unwrap_or(0)) / 2,
        y: (i16::try_from(h).unwrap_or(i16::MAX) - height as i16) / 2,
        width,
        height,
    }
}

fn confirm_rect(w: u16, h: u16) -> Rectangle {
    let width = 520.min(w.saturating_sub(80));
    let height = 150u16;
    Rectangle {
        x: (i16::try_from(w).unwrap_or(i16::MAX) - i16::try_from(width).unwrap_or(0)) / 2,
        y: (i16::try_from(h).unwrap_or(i16::MAX) - height as i16) / 2,
        width,
        height,
    }
}

fn confirm_buttons(w: u16, h: u16, count: usize) -> Vec<Rectangle> {
    let rect = confirm_rect(w, h);
    let count = count.max(1) as u16;
    let gap = 6u16;
    let width = (rect.width.saturating_sub(32 + gap * (count - 1))) / count;
    (0..count)
        .map(|index| Rectangle {
            x: rect.x + 16 + i16::try_from((width + gap) * index).unwrap_or(0),
            y: rect.y + i16::try_from(rect.height).unwrap_or(0) - 42,
            width,
            height: 28,
        })
        .collect()
}

/// U4: why a control is disabled, or `None` when it can be used. The
/// buttons are drawn disabled exactly when this has a reason, and the
/// reason is shown next to a hovered or focused disabled control.
fn disabled_reason(state: &State, hit: Hit) -> Option<&'static str> {
    let selected = state.current();
    let enabled = selected.is_some_and(|output| output.enabled);
    let control = !matches!(
        hit,
        Hit::Monitor(_) | Hit::Output(_) | Hit::Profile(_) | Hit::NameField | Hit::PopItem(_)
    );
    if state.busy && control {
        return Some("Working\u{2026} Esc closes the window");
    }
    match hit {
        Hit::Primary if !enabled => Some("Enable the screen to make it primary"),
        Hit::Enabled | Hit::Single if selected.is_none() => Some("Select a screen first"),
        Hit::Rotate if !enabled => Some("Enable the screen to rotate it"),
        Hit::Mode if !enabled => Some("Enable the screen to choose its mode"),
        Hit::Rate if !enabled => Some("Enable the screen to choose its refresh rate"),
        Hit::Extend if state.draft.len() < 2 => Some("Extend needs two connected screens"),
        Hit::Mirror if !enabled => Some("Enable the screen to mirror it"),
        Hit::Mirror if state.draft.iter().filter(|o| o.enabled).count() < 2 => {
            Some("Mirror needs two enabled screens")
        }
        Hit::Keep if !state.preview_active() => Some("Keep is available during a preview"),
        Hit::Keep if state.preview_changed() => {
            Some("The draft changed after the preview; press Preview again")
        }
        Hit::Revert if !state.preview_active() => Some("Revert is available during a preview"),
        Hit::Rename | Hit::Delete | Hit::Startup if state.selected_profile.is_none() => {
            Some("Select a profile first")
        }
        _ => None,
    }
}

/// Where an open list is drawn: under its button, or above it when the
/// window is too short, with the rows that fit.
#[derive(Clone, Debug)]
struct PopLayout {
    rect: Rectangle,
    rows: Vec<(usize, Rectangle)>,
}

fn popover_layout(state: &State, ui: &Ui, w: u16, h: u16) -> Option<PopLayout> {
    let popover = state.popover.as_ref()?;
    let anchor = focus_rect(
        ui,
        match popover.kind {
            PopKind::Mode => Hit::Mode,
            PopKind::Rate => Hit::Rate,
            PopKind::Rotation => Hit::Rotate,
        },
    )?;
    let (w, h) = (i32::from(w), i32::from(h));
    let width = i32::from(anchor.width).max(220).min(w - 8);
    let below = i32::from(anchor.y) + i32::from(anchor.height) + 2;
    let above = i32::from(anchor.y) - 2;
    let room_below = h - 4 - below;
    let room_above = above - 4;
    let room = room_below.max(room_above);
    let fit = ((room - 8) / i32::from(POP_ROW_H)).max(1) as usize;
    let visible = popover.items.len().min(POP_ROWS).min(fit);
    let height = visible as i32 * i32::from(POP_ROW_H) + 8;
    let y = if room_below >= height || room_below >= room_above {
        below
    } else {
        above - height
    };
    let x = i32::from(anchor.x).clamp(4, (w - width - 4).max(4));
    let first = popover
        .scroll
        .min(popover.items.len().saturating_sub(visible))
        .max((popover.cursor + 1).saturating_sub(visible))
        .min(popover.cursor);
    let rect = rect(x as i16, y as i16, width, height);
    let rows = (first..first + visible)
        .map(|index| {
            (
                index,
                Rectangle {
                    x: rect.x + 4,
                    y: rect.y + 4 + ((index - first) as i16) * POP_ROW_H,
                    width: rect.width.saturating_sub(8),
                    height: POP_ROW_H as u16,
                },
            )
        })
        .collect();
    Some(PopLayout { rect, rows })
}

fn hit(state: &State, ui: &Ui, x: i16, y: i16) -> Option<Hit> {
    if state.name_mode.is_some() {
        return Some(Hit::NameField);
    }
    if state.confirm.is_some() || ui.l.too_small {
        return None;
    }
    if state.popover.is_some() {
        // Only the list's own rows react while it is open.
        let w = (i32::from(ui.l.footer.x) * 2 + i32::from(ui.l.footer.width)) as u16;
        let h = (i32::from(ui.l.footer.y) + i32::from(ui.l.footer.height)) as u16;
        return popover_layout(state, ui, w, h).and_then(|layout| {
            layout
                .rows
                .iter()
                .find(|(_, row)| contains(*row, x, y))
                .map(|(index, _)| Hit::PopItem(*index))
        });
    }
    // D6: scrolled-out controls cannot be hit through the header or footer.
    if let Some(viewport) = ui.l.viewport {
        if !contains(viewport, x, y) {
            return None;
        }
    }
    for view in ui.monitors.iter().rev() {
        if contains(view.rect, x, y) {
            return Some(Hit::Monitor(view.index));
        }
    }
    for (index, rect) in &ui.panel.rows {
        if contains(*rect, x, y) {
            return Some(Hit::Output(*index));
        }
    }
    if let Some(rect) = ui.panel.page_prev {
        if contains(rect, x, y) {
            return Some(Hit::OutputPrev);
        }
    }
    if let Some(rect) = ui.panel.page_next {
        if contains(rect, x, y) {
            return Some(Hit::OutputNext);
        }
    }
    for (rect, action) in [
        (ui.panel.primary, Hit::Primary),
        (ui.panel.enabled, Hit::Enabled),
        (ui.panel.rotate, Hit::Rotate),
        (ui.panel.identify, Hit::Identify),
        (ui.panel.mode, Hit::Mode),
        (ui.panel.rate, Hit::Rate),
        (ui.panel.single, Hit::Single),
        (ui.panel.extend, Hit::Extend),
        (ui.panel.mirror, Hit::Mirror),
    ] {
        if contains(rect, x, y) {
            return Some(action);
        }
    }
    for (index, action) in [Hit::Apply, Hit::Keep, Hit::Revert].into_iter().enumerate() {
        if contains(ui.panel.apply[index], x, y) {
            return Some(action);
        }
    }
    for (index, rect) in &ui.profiles.rows {
        if contains(*rect, x, y) {
            return Some(Hit::Profile(*index));
        }
    }
    if let Some(rect) = ui.profiles.prev {
        if contains(rect, x, y) {
            return Some(Hit::ProfilePrev);
        }
    }
    if let Some(rect) = ui.profiles.next {
        if contains(rect, x, y) {
            return Some(Hit::ProfileNext);
        }
    }
    if let Some(rect) = ui.profiles.automatic {
        if contains(rect, x, y) {
            return Some(Hit::Automatic);
        }
    }
    for (index, action) in [
        Hit::Save,
        Hit::Rename,
        Hit::Delete,
        Hit::Startup,
        Hit::Reload,
    ]
    .into_iter()
    .enumerate()
    {
        if contains(ui.profiles.actions[index], x, y) {
            return Some(action);
        }
    }
    None
}

fn contains(rect: Rectangle, x: i16, y: i16) -> bool {
    x >= rect.x
        && x < rect.x + i16::try_from(rect.width).unwrap_or(0)
        && y >= rect.y
        && y < rect.y + i16::try_from(rect.height).unwrap_or(0)
}

fn focus_rect(ui: &Ui, focus: Hit) -> Option<Rectangle> {
    match focus {
        Hit::Monitor(index) => ui
            .monitors
            .iter()
            .find(|view| view.members.contains(&index))
            .map(|view| view.rect),
        Hit::Output(index) => ui
            .panel
            .rows
            .iter()
            .find(|(candidate, _)| *candidate == index)
            .map(|(_, rect)| *rect),
        Hit::OutputPrev => ui.panel.page_prev,
        Hit::OutputNext => ui.panel.page_next,
        Hit::Primary => Some(ui.panel.primary),
        Hit::Enabled => Some(ui.panel.enabled),
        Hit::Rotate => Some(ui.panel.rotate),
        Hit::Identify => Some(ui.panel.identify),
        Hit::Mode => Some(ui.panel.mode),
        Hit::Rate => Some(ui.panel.rate),
        Hit::PopItem(_) => None,
        Hit::Single => Some(ui.panel.single),
        Hit::Extend => Some(ui.panel.extend),
        Hit::Mirror => Some(ui.panel.mirror),
        Hit::Apply => Some(ui.panel.apply[0]),
        Hit::Keep => Some(ui.panel.apply[1]),
        Hit::Revert => Some(ui.panel.apply[2]),
        Hit::Profile(index) => ui
            .profiles
            .rows
            .iter()
            .find(|(candidate, _)| *candidate == index)
            .map(|(_, rect)| *rect),
        Hit::ProfilePrev => ui.profiles.prev,
        Hit::ProfileNext => ui.profiles.next,
        Hit::Automatic => ui.profiles.automatic,
        Hit::Save => Some(ui.profiles.actions[0]),
        Hit::Rename => Some(ui.profiles.actions[1]),
        Hit::Delete => Some(ui.profiles.actions[2]),
        Hit::Startup => Some(ui.profiles.actions[3]),
        Hit::Reload => Some(ui.profiles.actions[4]),
        Hit::NameField => None,
    }
}

fn focus_order(ui: &Ui) -> Vec<Hit> {
    let mut out: Vec<Hit> = ui
        .panel
        .rows
        .iter()
        .map(|(index, _)| Hit::Output(*index))
        .collect();
    if ui.panel.page_prev.is_some() {
        out.push(Hit::OutputPrev);
        out.push(Hit::OutputNext);
    }
    out.extend([
        Hit::Primary,
        Hit::Enabled,
        Hit::Rotate,
        Hit::Identify,
        Hit::Mode,
        Hit::Rate,
        Hit::Single,
        Hit::Extend,
        Hit::Mirror,
        Hit::Apply,
        Hit::Keep,
        Hit::Revert,
    ]);
    out.extend(
        ui.profiles
            .rows
            .iter()
            .map(|(index, _)| Hit::Profile(*index)),
    );
    if ui.profiles.prev.is_some() {
        out.push(Hit::ProfilePrev);
        out.push(Hit::ProfileNext);
    }
    if ui.profiles.automatic.is_some() {
        out.push(Hit::Automatic);
    }
    out.extend([
        Hit::Save,
        Hit::Rename,
        Hit::Delete,
        Hit::Startup,
        Hit::Reload,
    ]);
    out
}

#[derive(Clone, Debug)]
struct Drag {
    group: Vec<usize>,
    start_mouse: (i16, i16),
    start_positions: Vec<(usize, i32, i32)>,
    canvas: Canvas,
    moved: bool,
    was_dirty: bool,
    guide_x: Option<i32>,
    guide_y: Option<i32>,
}

#[derive(Clone, Copy)]
struct Cursors {
    normal: Cursor,
    fleur: Cursor,
    hand: Cursor,
}

/// Centre the first window on the screen. A floating dialog that starts at
/// 0,0 looks like a utility popup under the bar, not a settings window.
fn initial_position(screen_width: u16, screen_height: u16) -> (i16, i16) {
    let x = (i32::from(screen_width) - i32::from(WIDTH)) / 2;
    let y = (i32::from(screen_height) - i32::from(HEIGHT)) / 2;
    (x.max(0) as i16, y.max(0) as i16)
}

/// WM_NORMAL_HINTS for the window. Only the minimum size is advertised:
/// setting PMaxSize with a placeholder 1x1 made NIRUWM clamp the whole dialog
/// to a tiny window. No resize increments are needed for a pixel-layout UI.
fn window_size_hints() -> [u32; 18] {
    let mut hints = [0u32; 18];
    hints[0] = 1 << 4; // PMinSize
    hints[5] = u32::from(SMALLEST_WIDTH);
    hints[6] = u32::from(SMALLEST_HEIGHT);
    hints
}

pub struct App {
    c: RustConnection,
    win: Window,
    gc: Gcontext,
    back: Pixmap,
    w: u16,
    h: u16,
    root: Window,
    depth: u8,
    image_lsb: bool,
    pub palette: theme::Palette,
    font: text::Font,
    keys: keyboard::Keys,
    /// RandR change notifications are selected on the root window.
    randr: bool,
    wm_protocols: Atom,
    wm_delete_window: Atom,
    drag: Option<Drag>,
    pan_drag: Option<(i16, i16, f32, f32)>,
    zoom: f32,
    pan: (f32, f32),
    hover: Option<Hit>,
    cursors: Option<Cursors>,
    cursor: Option<Cursor>,
    overlays: Vec<Window>,
    identify_until: Option<Instant>,
    /// D6: how far the compact layout is scrolled.
    scroll: i16,
    /// U4: the hovered control's rectangle while painting.
    hover_rect: std::cell::Cell<Option<Rectangle>>,
    /// The last click, for the profile double-click.
    last_click: Option<(Hit, Instant)>,
    /// Painting is clipped to this (the compact viewport) when set.
    base_clip: std::cell::Cell<Option<Rectangle>>,
}

impl App {
    pub fn open() -> Result<Self, String> {
        let (c, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = c.setup().roots[screen].root;
        let root_geometry = c
            .get_geometry(root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let depth = root_geometry.depth;
        if ![24, 32].contains(&depth) {
            return Err(format!(
                "NIRU Display needs a 24 or 32-bit screen, this one is {depth}"
            ));
        }
        let image_lsb =
            c.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
        let palette = theme::load();
        let font = text::Font::load(palette);
        let keys = keyboard::Keys::load(&c)?;
        let win = c.generate_id().map_err(|e| e.to_string())?;
        let gc = c.generate_id().map_err(|e| e.to_string())?;
        let back = c.generate_id().map_err(|e| e.to_string())?;
        let (initial_x, initial_y) = initial_position(root_geometry.width, root_geometry.height);
        c.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            initial_x,
            initial_y,
            WIDTH,
            HEIGHT,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(palette.background)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::KEY_PRESS
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::POINTER_MOTION
                        | EventMask::STRUCTURE_NOTIFY,
                ),
        )
        .map_err(|e| e.to_string())?;
        c.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))
            .map_err(|e| e.to_string())?;
        c.create_pixmap(depth, back, root, WIDTH, HEIGHT)
            .map_err(|e| e.to_string())?;
        c.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            APP_NAME.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        let atom = |name: &str| -> Result<Atom, String> {
            c.intern_atom(false, name.as_bytes())
                .map_err(|e| e.to_string())?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|e| e.to_string())
        };
        c.change_property8(
            PropMode::REPLACE,
            win,
            atom("_NET_WM_NAME")?,
            atom("UTF8_STRING")?,
            APP_NAME.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        c.change_property8(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            format!("{APP_ID}\0{APP_ID}\0").as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atom("_NET_WM_PID")?,
            AtomEnum::CARDINAL,
            &[std::process::id()],
        )
        .map_err(|e| e.to_string())?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atom("_NET_WM_WINDOW_TYPE")?,
            AtomEnum::ATOM,
            &[atom("_NET_WM_WINDOW_TYPE_DIALOG")?],
        )
        .map_err(|e| e.to_string())?;
        let hints = window_size_hints();
        c.change_property32(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            &hints,
        )
        .map_err(|e| e.to_string())?;
        // Interned once: the close handler compares against these.
        let wm_protocols = atom("WM_PROTOCOLS")?;
        let wm_delete_window = atom("WM_DELETE_WINDOW")?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            wm_protocols,
            AtomEnum::ATOM,
            &[wm_delete_window],
        )
        .map_err(|e| e.to_string())?;
        let cursors = load_cursors(&c);
        // D1: hotplug and layout changes arrive as RandR events instead of
        // a status read (and an xrandr reprobe) every two seconds.
        let randr = c
            .randr_query_version(1, 3)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some()
            && c.randr_select_input(
                root,
                randr::NotifyMask::SCREEN_CHANGE
                    | randr::NotifyMask::OUTPUT_CHANGE
                    | randr::NotifyMask::CRTC_CHANGE
                    | randr::NotifyMask::OUTPUT_PROPERTY,
            )
            .map(|cookie| cookie.check().is_ok())
            .unwrap_or(false);
        c.map_window(win).map_err(|e| e.to_string())?;
        c.sync().map_err(|e| e.to_string())?;
        Ok(Self {
            c,
            win,
            gc,
            back,
            w: WIDTH,
            h: HEIGHT,
            root,
            depth,
            image_lsb,
            palette,
            font,
            keys,
            randr,
            wm_protocols,
            wm_delete_window,
            drag: None,
            pan_drag: None,
            zoom: 1.0,
            pan: (0.0, 0.0),
            hover: None,
            cursors,
            cursor: None,
            overlays: Vec::new(),
            identify_until: None,
            scroll: 0,
            hover_rect: std::cell::Cell::new(None),
            last_click: None,
            base_clip: std::cell::Cell::new(None),
        })
    }

    /// The layout of everything at the window's current size and view.
    fn ui(&self, state: &State) -> Ui {
        Ui::scrolled(
            state,
            self.w,
            self.h,
            self.drag.as_ref().map(|drag| drag.canvas),
            self.zoom,
            self.pan,
            self.scroll,
        )
    }

    /// Scroll the compact content by `delta` pixels.
    fn scroll_by(&mut self, delta: i16) {
        let max = layout(self.w, self.h).max_scroll;
        self.scroll = self.scroll.saturating_add(delta).clamp(0, max);
    }

    /// Scroll the compact content so the focused control is in view.
    fn reveal(&mut self, state: &State) {
        let ui = self.ui(state);
        let (Some(viewport), Some(rect)) =
            (ui.l.viewport, state.focus.and_then(|f| focus_rect(&ui, f)))
        else {
            return;
        };
        let top = i32::from(viewport.y) + 4;
        let bottom = i32::from(viewport.y) + i32::from(viewport.height) - 4;
        let y0 = i32::from(rect.y);
        let y1 = y0 + i32::from(rect.height);
        if y0 < top {
            self.scroll_by((y0 - top) as i16);
        } else if y1 > bottom {
            self.scroll_by((y1 - bottom).min(y0 - top) as i16);
        }
    }

    /// The RandR fingerprint (see `watch::Stamps`), read without probing
    /// the outputs: `GetScreenResourcesCurrent` and the CRTC and output
    /// details at its configuration time.
    fn stamps(&self) -> Option<watch::Stamps> {
        use std::hash::{Hash, Hasher};
        if !self.randr {
            return None;
        }
        let res = self
            .c
            .randr_get_screen_resources_current(self.root)
            .ok()?
            .reply()
            .ok()?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        (res.timestamp, res.config_timestamp).hash(&mut hash);
        if let Some(geometry) = self
            .c
            .get_geometry(self.root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        {
            (geometry.width, geometry.height).hash(&mut hash);
        }
        let crtcs: Vec<_> = res
            .crtcs
            .iter()
            .filter_map(|crtc| self.c.randr_get_crtc_info(*crtc, res.config_timestamp).ok())
            .collect();
        for cookie in crtcs {
            if let Ok(info) = cookie.reply() {
                (
                    info.x,
                    info.y,
                    info.width,
                    info.height,
                    info.mode,
                    u16::from(info.rotation),
                    info.outputs,
                )
                    .hash(&mut hash);
            }
        }
        let outputs: Vec<_> = res
            .outputs
            .iter()
            .filter_map(|output| {
                self.c
                    .randr_get_output_info(*output, res.config_timestamp)
                    .ok()
            })
            .collect();
        for cookie in outputs {
            if let Ok(info) = cookie.reply() {
                (u8::from(info.connection), info.crtc, info.modes).hash(&mut hash);
            }
        }
        Some(hash.finish())
    }

    fn resize_backing(&mut self) -> Result<(), String> {
        let pixmap = self.c.generate_id().map_err(|e| e.to_string())?;
        self.c
            .create_pixmap(self.depth, pixmap, self.root, self.w, self.h)
            .map_err(|e| e.to_string())?;
        let old = std::mem::replace(&mut self.back, pixmap);
        let _ = self.c.free_pixmap(old);
        Ok(())
    }

    fn fill(&self, colour: u32, rect: Rectangle) -> Result<(), String> {
        if rect.width == 0 || rect.height == 0 {
            return Ok(());
        }
        self.c
            .change_gc(self.gc, &ChangeGCAux::new().foreground(colour))
            .map_err(|e| e.to_string())?;
        self.c
            .poly_fill_rectangle(self.back, self.gc, &[rect])
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    fn outline(&self, rect: Rectangle, thickness: u16, colour: u32) -> Result<(), String> {
        if rect.width == 0 || rect.height == 0 || thickness == 0 {
            return Ok(());
        }
        let t = thickness.min(rect.width).min(rect.height) as i16;
        let width = i16::try_from(rect.width).unwrap_or(i16::MAX);
        let height = i16::try_from(rect.height).unwrap_or(i16::MAX);
        self.fill(
            colour,
            Rectangle {
                width: rect.width,
                height: thickness,
                ..rect
            },
        )?;
        self.fill(
            colour,
            Rectangle {
                y: rect.y + height - t,
                width: rect.width,
                height: thickness,
                ..rect
            },
        )?;
        self.fill(
            colour,
            Rectangle {
                width: thickness,
                height: rect.height,
                ..rect
            },
        )?;
        self.fill(
            colour,
            Rectangle {
                x: rect.x + width - t,
                width: thickness,
                height: rect.height,
                ..rect
            },
        )
    }

    fn dashed_outline(&self, rect: Rectangle, colour: u32) -> Result<(), String> {
        if rect.width < 2 || rect.height < 2 {
            return Ok(());
        }
        let dash = 5i16;
        let gap = 3i16;
        let width = i16::try_from(rect.width).unwrap_or(i16::MAX);
        let height = i16::try_from(rect.height).unwrap_or(i16::MAX);
        let mut x = 0;
        while x < width {
            let step = dash.min(width - x);
            self.fill(
                colour,
                Rectangle {
                    x: rect.x + x,
                    y: rect.y,
                    width: step as u16,
                    height: 1,
                },
            )?;
            self.fill(
                colour,
                Rectangle {
                    x: rect.x + x,
                    y: rect.y + height - 1,
                    width: step as u16,
                    height: 1,
                },
            )?;
            x += dash + gap;
        }
        let mut y = 0;
        while y < height {
            let step = dash.min(height - y);
            self.fill(
                colour,
                Rectangle {
                    x: rect.x,
                    y: rect.y + y,
                    width: 1,
                    height: step as u16,
                },
            )?;
            self.fill(
                colour,
                Rectangle {
                    x: rect.x + width - 1,
                    y: rect.y + y,
                    width: 1,
                    height: step as u16,
                },
            )?;
            y += dash + gap;
        }
        Ok(())
    }

    fn upload_to(
        &self,
        target: Window,
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        pixels: &[u8],
    ) -> Result<(), String> {
        let bgr = self.image_lsb;
        let mut data = Vec::with_capacity(pixels.len());
        for chunk in pixels.chunks_exact(4) {
            if bgr {
                data.extend([chunk[2], chunk[1], chunk[0], 255]);
            } else {
                data.extend([chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        self.c
            .put_image(
                ImageFormat::Z_PIXMAP,
                target,
                self.gc,
                w,
                h,
                x,
                y,
                0,
                self.depth,
                data.as_slice(),
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    fn label_on(
        &self,
        x: i16,
        y: i16,
        text: &str,
        colour: u32,
        bg: u32,
        size: f32,
    ) -> Result<(), String> {
        let (w, h, pixels) = self.font.size(size).render(text, colour, bg);
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.upload_to(self.back, x, y, w as u16, h as u16, &pixels)
    }

    fn label(&self, x: i16, y: i16, text: &str, colour: u32, size: f32) -> Result<(), String> {
        self.label_on(x, y, text, colour, self.palette.background, size)
    }

    fn text_width(&self, text: &str, size: f32) -> f32 {
        self.font.size(size).width(text)
    }

    fn fit_text(&self, text: &str, size: f32, max_width: f32) -> String {
        if self.text_width(text, size) <= max_width {
            return text.to_string();
        }
        let ellipsis = "…";
        let mut out = String::new();
        for ch in text.chars() {
            let mut candidate = out.clone();
            candidate.push(ch);
            candidate.push_str(ellipsis);
            if self.text_width(&candidate, size) > max_width {
                break;
            }
            out.push(ch);
        }
        out.push_str(ellipsis);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn label_fit(
        &self,
        x: i16,
        y: i16,
        text: &str,
        colour: u32,
        bg: u32,
        size: f32,
        max_width: f32,
    ) -> Result<(), String> {
        if max_width < 8.0 {
            return Ok(());
        }
        let text = self.fit_text(text, size, max_width);
        self.label_on(x, y, &text, colour, bg, size)
    }

    fn label_right(
        &self,
        right: i16,
        y: i16,
        text: &str,
        colour: u32,
        size: f32,
    ) -> Result<(), String> {
        let width = self.text_width(text, size).ceil() as i16;
        self.label(right - width, y, text, colour, size)
    }

    fn wrap_text(&self, text: &str, size: f32, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut current = String::new();
        for word in text.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if !current.is_empty() && self.text_width(&candidate, size) > max_width {
                lines.push(current);
                current = word.to_string();
            } else {
                current = candidate;
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }

    fn button(
        &self,
        rect: Rectangle,
        text: &str,
        active: bool,
        enabled: bool,
        bg: u32,
    ) -> Result<(), String> {
        let hovered = self.hover_rect.get().is_some_and(|hover| {
            hover.x == rect.x
                && hover.y == rect.y
                && hover.width == rect.width
                && hover.height == rect.height
        });
        let (fill, fg) = if !enabled {
            (self.palette.background, self.palette.border)
        } else if active && hovered {
            (
                theme::Palette::blend(self.palette.accent, self.palette.foreground, 0.15),
                self.palette.background,
            )
        } else if active {
            (self.palette.accent, self.palette.background)
        } else if hovered {
            // U4: a hovered button lifts towards the foreground.
            (
                theme::Palette::blend(bg, self.palette.foreground, 0.10),
                self.palette.foreground,
            )
        } else {
            (bg, self.palette.foreground)
        };
        self.fill(fill, rect)?;
        self.outline(
            rect,
            1,
            if enabled {
                self.palette.frame()
            } else {
                self.palette.border
            },
        )?;
        let (_, text_h, _) = self.font.size(13.0).render(text, fg, fill);
        let y = rect.y + (i16::try_from(rect.height).unwrap_or(0) - text_h as i16) / 2;
        let padding = if rect.width < 40 { 3 } else { 8 };
        self.label_fit(
            rect.x + padding,
            y.max(rect.y),
            text,
            fg,
            fill,
            if rect.width < 40 { 11.0 } else { 13.0 },
            f32::from(rect.width).max(1.0) - f32::from(padding * 2),
        )
    }

    fn badge(&self, rect: Rectangle, text: &str, fill: u32, fg: u32) -> Result<(), String> {
        self.fill(fill, rect)?;
        let (_, text_h, _) = self.font.size(12.0).render(text, fg, fill);
        let y = rect.y + (i16::try_from(rect.height).unwrap_or(0) - text_h as i16) / 2;
        self.label_fit(
            rect.x + 4,
            y.max(rect.y),
            text,
            fg,
            fill,
            12.0,
            f32::from(rect.width).max(1.0) - 6.0,
        )
    }

    fn ring_coloured(&self, rect: Rectangle, colour: u32) -> Result<(), String> {
        let outer = Rectangle {
            x: rect.x - 2,
            y: rect.y - 2,
            width: rect.width + 4,
            height: rect.height + 4,
        };
        self.outline(outer, 2, colour)
    }

    fn clip(&self, rect: Rectangle) -> Result<(), String> {
        let rect = match self.base_clip.get() {
            Some(base) => intersect(rect, base),
            None => rect,
        };
        self.c
            .set_clip_rectangles(ClipOrdering::UNSORTED, self.gc, 0, 0, &[rect])
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    /// Back to the base clip: the compact viewport, or the whole window.
    fn unclip(&self) -> Result<(), String> {
        let full = Rectangle {
            x: 0,
            y: 0,
            width: self.w,
            height: self.h,
        };
        self.c
            .set_clip_rectangles(
                ClipOrdering::UNSORTED,
                self.gc,
                0,
                0,
                &[self.base_clip.get().unwrap_or(full)],
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    fn set_base_clip(&self, rect: Option<Rectangle>) -> Result<(), String> {
        self.base_clip.set(rect);
        self.unclip()
    }

    fn set_cursor(&mut self, cursor: Option<Cursor>) -> Result<(), String> {
        if cursor == self.cursor {
            return Ok(());
        }
        if let Some(cursor) = cursor {
            self.c
                .change_window_attributes(
                    self.win,
                    &ChangeWindowAttributesAux::new().cursor(cursor),
                )
                .map_err(|e| e.to_string())?;
            self.cursor = Some(cursor);
        }
        Ok(())
    }

    fn identify(&mut self, state: &State) -> Result<(), String> {
        self.clear_identify()?;
        let mut groups: Vec<(Rectangle, Vec<String>)> = Vec::new();
        for output in &state.status.outputs {
            if output.current.is_none() {
                continue;
            }
            let Some(geometry) = &output.geometry else {
                continue;
            };
            let rect = Rectangle {
                x: geometry.x.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                y: geometry.y.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
                width: geometry.width.clamp(1, i32::from(u16::MAX)) as u16,
                height: geometry.height.clamp(1, i32::from(u16::MAX)) as u16,
            };
            let number = state
                .draft
                .iter()
                .position(|candidate| candidate.name == output.name)
                .map(|index| index + 1)
                .unwrap_or(0);
            let model = Self::identity_detail(state, &output.name);
            let label = if model.is_empty() {
                format!("{number}  {}", output.name)
            } else {
                format!("{number}  {}  {}", output.name, model)
            };
            if let Some((_, labels)) = groups.iter_mut().find(|(candidate, _)| {
                candidate.x == rect.x
                    && candidate.y == rect.y
                    && candidate.width == rect.width
                    && candidate.height == rect.height
            }) {
                labels.push(label);
            } else {
                groups.push((rect, vec![label]));
            }
        }
        if groups.is_empty() {
            return Err("No active screen to identify.".into());
        }
        let opacity = self
            .c
            .intern_atom(false, b"_NET_WM_WINDOW_OPACITY")
            .map_err(|e| e.to_string())?
            .reply()
            .map(|reply| reply.atom)
            .map_err(|e| e.to_string())?;
        for (rect, labels) in groups {
            let win = self.c.generate_id().map_err(|e| e.to_string())?;
            let bg = self.palette.accent;
            self.c
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    win,
                    self.root,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .override_redirect(1)
                        .background_pixel(bg),
                )
                .map_err(|e| e.to_string())?;
            self.c
                .change_property32(
                    PropMode::REPLACE,
                    win,
                    opacity,
                    AtomEnum::CARDINAL,
                    &[u32::MAX],
                )
                .map_err(|e| e.to_string())?;
            self.c.map_window(win).map_err(|e| e.to_string())?;
            self.c
                .configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))
                .map_err(|e| e.to_string())?;
            self.c
                .set_clip_rectangles(
                    ClipOrdering::UNSORTED,
                    self.gc,
                    0,
                    0,
                    &[Rectangle {
                        x: 0,
                        y: 0,
                        width: rect.width,
                        height: rect.height,
                    }],
                )
                .map_err(|e| e.to_string())?
                .check()
                .map_err(|e| e.to_string())?;
            let mut size = (f32::from(rect.height) / (labels.len() as f32 + 2.0)).clamp(24.0, 96.0);
            let max_width = (f32::from(rect.width) - 40.0).max(1.0);
            while size > 18.0
                && labels
                    .iter()
                    .any(|label| self.text_width(label, size) > max_width)
            {
                size *= 0.9;
            }
            let gap = (size as i32 / 3).max(4);
            let heights: Vec<i32> = labels
                .iter()
                .map(|label| {
                    self.font
                        .size(size)
                        .render(label, self.palette.background, bg)
                        .1 as i32
                })
                .collect();
            let total = heights.iter().sum::<i32>() + gap * labels.len().saturating_sub(1) as i32;
            let mut y = (i32::from(rect.height) - total) / 2;
            for (label, height) in labels.iter().zip(heights) {
                let label = self.fit_text(label, size, max_width);
                let (w, h, pixels) =
                    self.font
                        .size(size)
                        .render(&label, self.palette.background, bg);
                let x = (i32::from(rect.width) - w as i32) / 2;
                self.upload_to(
                    win,
                    x.max(0) as i16,
                    y.max(0) as i16,
                    w as u16,
                    h as u16,
                    &pixels,
                )?;
                y += height.max(h as i32) + gap;
            }
            self.overlays.push(win);
        }
        self.unclip()?;
        self.identify_until = Some(Instant::now() + Duration::from_secs(4));
        self.c.flush().map_err(|e| e.to_string())
    }

    fn clear_identify(&mut self) -> Result<(), String> {
        for window in self.overlays.drain(..) {
            let _ = self.c.destroy_window(window);
        }
        self.identify_until = None;
        self.c.flush().map_err(|e| e.to_string())
    }

    fn identity_text(state: &State, name: &str) -> String {
        state
            .status
            .output(name)
            .and_then(|output| output.identity.as_ref())
            .map(|identity| {
                let mut text = identity.manufacturer.clone();
                if let Some(model) = &identity.model {
                    text.push(' ');
                    text.push_str(model);
                }
                text
            })
            .unwrap_or_default()
    }

    fn identity_detail(state: &State, name: &str) -> String {
        state
            .status
            .output(name)
            .and_then(|output| output.identity.as_ref())
            .map(|identity| {
                let mut text = identity.manufacturer.clone();
                if let Some(model) = &identity.model {
                    text.push(' ');
                    text.push_str(model);
                }
                if let Some(serial) = &identity.serial {
                    let serial = serial.trim();
                    if !serial.is_empty() {
                        let tail: String = serial
                            .chars()
                            .rev()
                            .take(8)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        text.push_str(" · SN ");
                        text.push_str(&tail);
                    }
                }
                text
            })
            .unwrap_or_default()
    }

    fn paint(&self, state: &State, ui: &Ui, help: bool) -> Result<(), String> {
        self.fill(
            self.palette.background,
            Rectangle {
                x: 0,
                y: 0,
                width: self.w,
                height: self.h,
            },
        )?;
        if ui.l.too_small {
            return self.paint_too_small(state, ui);
        }
        let compact = ui.l.viewport.is_some();
        let (title_y, subtitle_y) = if compact {
            (4, 30)
        } else {
            (PAD - 8, PAD + 22)
        };
        self.label(PAD, title_y, APP_NAME, self.palette.foreground, 20.0)?;
        let subtitle = format!(
            "{} connected screens    {} saved profiles",
            state
                .status
                .outputs
                .iter()
                .filter(|output| output.connected)
                .count(),
            state.status.profiles.len()
        );
        self.label(PAD, subtitle_y, &subtitle, self.palette.muted, 12.0)?;
        if state.dirty {
            self.label_right(
                i16::try_from(self.w).unwrap_or(i16::MAX) - PAD,
                subtitle_y,
                "Unsaved changes",
                self.palette.warning(),
                12.0,
            )?;
        }
        if let Some(left) = state.preview_seconds_left(Instant::now()) {
            self.label_right(
                i16::try_from(self.w).unwrap_or(i16::MAX) - PAD,
                title_y + 4,
                &format!("Preview {left} s"),
                self.palette.accent,
                13.0,
            )?;
        }
        self.hover_rect.set(
            self.hover
                .filter(|_| self.drag.is_none())
                .and_then(|hover| focus_rect(ui, hover)),
        );
        // D6: the compact content is clipped to the scrolling viewport.
        self.set_base_clip(ui.l.viewport)?;
        self.paint_canvas(state, ui)?;
        self.paint_panel(state, ui)?;
        self.paint_profiles(state, ui)?;
        if let Some(focus) = state.focus {
            if let Some(rect) = focus_rect(ui, focus) {
                self.ring_coloured(rect, self.palette.foreground)?;
            }
        }
        if let Some(hover) = self.hover {
            if Some(hover) != state.focus && self.drag.is_none() {
                if let Some(rect) = focus_rect(ui, hover) {
                    self.ring_coloured(rect, self.palette.frame())?;
                }
            }
        }
        self.set_base_clip(None)?;
        if let Some(viewport) = ui.l.viewport {
            self.paint_scrollbar(viewport, ui.l.max_scroll)?;
        }
        self.paint_popover(state, ui)?;
        if state.popover.is_none() && state.name_mode.is_none() && state.confirm.is_none() && !help
        {
            self.paint_tip(state, ui)?;
        }
        if state.name_mode.is_some() {
            self.paint_name(state)?;
        } else if state.confirm.is_some() {
            self.paint_confirm(state)?;
        } else if help {
            self.paint_help()?;
        }
        self.paint_footer(state, ui, help)?;
        Ok(())
    }

    /// D6: below the smallest supported size nothing is clipped into
    /// uselessness; the window says what to do instead.
    fn paint_too_small(&self, state: &State, ui: &Ui) -> Result<(), String> {
        let w = i16::try_from(self.w).unwrap_or(i16::MAX);
        let h = i16::try_from(self.h).unwrap_or(i16::MAX);
        let text = "Make the window larger";
        let width = self.text_width(text, 16.0).ceil() as i16;
        self.label(
            ((w - width) / 2).max(4),
            (h / 2 - 24).max(4),
            text,
            self.palette.foreground,
            16.0,
        )?;
        let hint = format!("NIRU Display needs at least {SMALLEST_WIDTH}x{SMALLEST_HEIGHT}.");
        let width = self.text_width(&hint, 11.0).ceil() as i16;
        self.label(
            ((w - width) / 2).max(4),
            h / 2 + 2,
            &hint,
            self.palette.muted,
            11.0,
        )?;
        if state.confirm.is_some() {
            self.paint_confirm(state)?;
        } else if state.name_mode.is_some() {
            self.paint_name(state)?;
        }
        let _ = ui;
        Ok(())
    }

    /// U4: a short line next to a hovered (or else focused) disabled
    /// control that says why it is disabled.
    fn paint_tip(&self, state: &State, ui: &Ui) -> Result<(), String> {
        let target = self
            .hover
            .filter(|_| self.drag.is_none())
            .and_then(|hit| disabled_reason(state, hit).map(|why| (hit, why)))
            .or_else(|| {
                state
                    .focus
                    .and_then(|hit| disabled_reason(state, hit).map(|why| (hit, why)))
            });
        let Some((hit, why)) = target else {
            return Ok(());
        };
        let Some(anchor) = focus_rect(ui, hit) else {
            return Ok(());
        };
        let width = (self.text_width(why, 11.0).ceil() as i32 + 12).min(i32::from(self.w) - 8);
        let height = 20;
        let x = i32::from(anchor.x).clamp(4, (i32::from(self.w) - width - 4).max(4));
        let above = i32::from(anchor.y) - height - 3;
        let y = if above >= 4 {
            above
        } else {
            i32::from(anchor.y) + i32::from(anchor.height) + 3
        };
        let tip = rect(x as i16, y as i16, width, height);
        self.fill(self.palette.surface, tip)?;
        self.outline(tip, 1, self.palette.frame())?;
        self.label_fit(
            tip.x + 6,
            tip.y + 3,
            why,
            self.palette.foreground,
            self.palette.surface,
            11.0,
            f32::from(tip.width) - 10.0,
        )
    }

    /// U1: the open mode, rate or rotation list.
    fn paint_popover(&self, state: &State, ui: &Ui) -> Result<(), String> {
        let (Some(popover), Some(layout)) =
            (&state.popover, popover_layout(state, ui, self.w, self.h))
        else {
            return Ok(());
        };
        self.fill(self.palette.background, layout.rect)?;
        self.outline(layout.rect, 1, self.palette.accent)?;
        let check = if self.font.has('\u{2713}') {
            "\u{2713}"
        } else {
            "*"
        };
        for (index, row) in &layout.rows {
            let Some(item) = popover.items.get(*index) else {
                continue;
            };
            let bg = if *index == popover.cursor {
                theme::Palette::blend(self.palette.background, self.palette.accent, 0.35)
            } else if self.hover == Some(Hit::PopItem(*index)) {
                theme::Palette::blend(self.palette.background, self.palette.foreground, 0.08)
            } else {
                self.palette.background
            };
            self.fill(bg, *row)?;
            let y = row.y + 4;
            if item.checked {
                self.label_on(row.x + 6, y, check, self.palette.accent, bg, 13.0)?;
            }
            self.label_fit(
                row.x + 24,
                y,
                &item.label,
                self.palette.foreground,
                bg,
                13.0,
                f32::from(row.width) - 30.0,
            )?;
            if let Some(note) = item.note {
                let width = self.text_width(note, 10.0).ceil() as i16;
                self.label_on(
                    row.x + i16::try_from(row.width).unwrap_or(0) - width - 8,
                    y + 2,
                    note,
                    self.palette.muted,
                    bg,
                    10.0,
                )?;
            }
        }
        // More entries than rows: a small marker at the edge that has more.
        let first = layout.rows.first().map(|(index, _)| *index).unwrap_or(0);
        let last = layout.rows.last().map(|(index, _)| *index).unwrap_or(0);
        let marker = |y: i16| Rectangle {
            x: layout.rect.x + i16::try_from(layout.rect.width).unwrap_or(0) - 5,
            y,
            width: 2,
            height: 8,
        };
        if first > 0 {
            self.fill(self.palette.muted, marker(layout.rect.y + 3))?;
        }
        if last + 1 < popover.items.len() {
            self.fill(
                self.palette.muted,
                marker(layout.rect.y + i16::try_from(layout.rect.height).unwrap_or(0) - 11),
            )?;
        }
        Ok(())
    }

    /// A thin bar at the right edge of the compact viewport.
    fn paint_scrollbar(&self, viewport: Rectangle, max_scroll: i16) -> Result<(), String> {
        if max_scroll <= 0 {
            return Ok(());
        }
        let track = Rectangle {
            x: viewport.x + i16::try_from(viewport.width).unwrap_or(0) - 6,
            y: viewport.y + 2,
            width: 3,
            height: viewport.height.saturating_sub(4),
        };
        self.fill(self.palette.border, track)?;
        let view = f32::from(viewport.height);
        let total = view + f32::from(max_scroll);
        let thumb_h = (f32::from(track.height) * view / total).max(18.0);
        let travel = f32::from(track.height) - thumb_h;
        let y = f32::from(track.y)
            + travel * (f32::from(self.scroll.clamp(0, max_scroll)) / f32::from(max_scroll));
        self.fill(
            self.palette.muted,
            Rectangle {
                y: y.round() as i16,
                height: thumb_h.round() as u16,
                ..track
            },
        )
    }

    fn paint_canvas(&self, state: &State, ui: &Ui) -> Result<(), String> {
        self.fill(self.palette.background, ui.l.canvas)?;
        self.outline(ui.l.canvas, 1, self.palette.border)?;
        self.clip(ui.l.canvas)?;
        for view in &ui.monitors {
            self.paint_monitor(state, view)?;
        }
        if state.draft.is_empty() {
            self.label(
                ui.l.canvas.x + 20,
                ui.l.canvas.y + 24,
                "No connected screen reports a usable mode.",
                self.palette.muted,
                13.0,
            )?;
        } else if state.draft.iter().filter(|output| output.enabled).count() == 0 {
            self.label(
                ui.l.canvas.x + 20,
                ui.l.canvas.y + 24,
                "No screen is enabled. Select one in the panel and enable it.",
                self.palette.muted,
                13.0,
            )?;
        }
        if let Some(drag) = &self.drag {
            if drag.moved {
                if let Some(guide) = drag.guide_x {
                    let x = (ui.canvas.x + guide as f32 * ui.canvas.scale).round() as i16;
                    self.fill(
                        self.palette.accent,
                        Rectangle {
                            x,
                            y: ui.l.canvas.y,
                            width: 1,
                            height: ui.l.canvas.height,
                        },
                    )?;
                }
                if let Some(guide) = drag.guide_y {
                    let y = (ui.canvas.y + guide as f32 * ui.canvas.scale).round() as i16;
                    self.fill(
                        self.palette.accent,
                        Rectangle {
                            x: ui.l.canvas.x,
                            y,
                            width: ui.l.canvas.width,
                            height: 1,
                        },
                    )?;
                }
                if let Some(view) = ui
                    .monitors
                    .iter()
                    .find(|view| drag.group.contains(&view.index))
                {
                    let output = state.draft.get(view.index);
                    if let Some(output) = output {
                        let text = format!("x {}   y {}", output.x, output.y);
                        let width = self.text_width(&text, 12.0).ceil() as u16 + 12;
                        let min_x = ui.l.canvas.x + 2;
                        let max_x = (ui.l.canvas.x + i16::try_from(ui.l.canvas.width).unwrap_or(0)
                            - i16::try_from(width).unwrap_or(0)
                            - 2)
                        .max(min_x);
                        let x = view.rect.x.clamp(min_x, max_x);
                        let y = (view.rect.y + i16::try_from(view.rect.height).unwrap_or(0) + 4)
                            .min(
                                ui.l.canvas.y + i16::try_from(ui.l.canvas.height).unwrap_or(0) - 24,
                            );
                        let rect = Rectangle {
                            x,
                            y,
                            width,
                            height: 20,
                        };
                        self.fill(self.palette.surface, rect)?;
                        self.outline(rect, 1, self.palette.accent)?;
                        self.label_on(
                            rect.x + 6,
                            rect.y + 3,
                            &text,
                            self.palette.foreground,
                            self.palette.surface,
                            12.0,
                        )?;
                    }
                }
            }
        }
        self.unclip()?;
        if (self.zoom - 1.0).abs() > 0.001 {
            let text = format!("{}%", (self.zoom * 100.0).round() as i32);
            let width = self.text_width(&text, 11.0).ceil() as u16 + 10;
            let rect = Rectangle {
                x: ui.l.canvas.x + i16::try_from(ui.l.canvas.width).unwrap_or(0)
                    - i16::try_from(width).unwrap_or(0)
                    - 6,
                y: ui.l.canvas.y + 6,
                width,
                height: 18,
            };
            self.fill(self.palette.surface, rect)?;
            self.outline(rect, 1, self.palette.accent)?;
            self.label_on(
                rect.x + 5,
                rect.y + 2,
                &text,
                self.palette.foreground,
                self.palette.surface,
                11.0,
            )?;
        }
        let issues = validate_draft(&state.draft);
        if let Some(issue) = issues
            .iter()
            .find(|issue| issue.blocking)
            .or_else(|| issues.first())
        {
            let colour = if issue.blocking {
                self.palette.error()
            } else {
                self.palette.warning()
            };
            let rect = Rectangle {
                x: ui.l.canvas.x + 1,
                y: ui.l.canvas.y + i16::try_from(ui.l.canvas.height).unwrap_or(0) - 23,
                width: ui.l.canvas.width.saturating_sub(2),
                height: 22,
            };
            self.fill(
                theme::Palette::blend(self.palette.background, colour, 0.18),
                rect,
            )?;
            self.label_fit(
                rect.x + 8,
                rect.y + 4,
                &issue.text,
                colour,
                theme::Palette::blend(self.palette.background, colour, 0.18),
                11.0,
                f32::from(rect.width) - 16.0,
            )?;
        }
        Ok(())
    }

    fn paint_monitor(&self, state: &State, view: &MonitorView) -> Result<(), String> {
        let Some(first) = state.draft.get(view.members[0]) else {
            return Ok(());
        };
        let hovered = self.drag.is_none()
            && matches!(self.hover, Some(Hit::Monitor(index)) if view.members.contains(&index));
        let bg = if !view.enabled {
            self.palette.background
        } else if view.selected {
            self.palette.screen_selected()
        } else {
            self.palette.screen()
        };
        // U4: a hovered screen lifts a little towards the foreground.
        let bg = if hovered {
            theme::Palette::blend(bg, self.palette.foreground, 0.06)
        } else {
            bg
        };
        let frame = if view.overlapping {
            self.palette.error()
        } else if view.selected {
            self.palette.accent
        } else if view.primary {
            theme::Palette::blend(self.palette.accent, self.palette.foreground, 0.35)
        } else {
            self.palette.frame()
        };
        self.fill(bg, view.rect)?;
        if view.enabled {
            self.outline(view.rect, 2, frame)?;
        } else {
            self.dashed_outline(view.rect, self.palette.frame())?;
        }
        if view.selected {
            self.ring_coloured(view.rect, self.palette.accent)?;
        }
        let numbers = view
            .members
            .iter()
            .map(|index| (index + 1).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let badge_width = (self.text_width(&numbers, 12.0).ceil() as u16 + 10).max(20);
        if view.rect.width >= badge_width + 12 && view.rect.height >= 24 {
            self.badge(
                Rectangle {
                    x: view.rect.x + 6,
                    y: view.rect.y + 6,
                    width: badge_width,
                    height: 18,
                },
                &numbers,
                if view.selected {
                    self.palette.accent
                } else {
                    self.palette.surface
                },
                if view.selected {
                    self.palette.background
                } else {
                    self.palette.foreground
                },
            )?;
        }
        let names = view
            .members
            .iter()
            .filter_map(|index| state.draft.get(*index))
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>()
            .join(" + ");
        let name_x = view.rect.x + 10 + i16::try_from(badge_width).unwrap_or(0);
        let name_width =
            f32::from(view.rect.width).max(1.0) - f32::from(name_x - view.rect.x) - 10.0;
        if view.rect.height >= 24 {
            self.label_fit(
                name_x,
                view.rect.y + 7,
                &names,
                if view.enabled {
                    self.palette.foreground
                } else {
                    self.palette.muted
                },
                bg,
                13.0,
                name_width,
            )?;
        }
        if view.primary && view.rect.width > 54 {
            let badge = Rectangle {
                x: view.rect.x + i16::try_from(view.rect.width).unwrap_or(0) - 24,
                y: view.rect.y + 6,
                width: 18,
                height: 18,
            };
            self.badge(badge, "P", self.palette.accent, self.palette.background)?;
        }
        if !view.enabled {
            if view.rect.height >= 48 {
                self.label_fit(
                    view.rect.x + 10,
                    view.rect.y + 30,
                    "off",
                    self.palette.muted,
                    bg,
                    11.0,
                    f32::from(view.rect.width).max(1.0) - 20.0,
                )?;
            }
            return Ok(());
        }
        if view.rect.height >= 48 {
            let rate = first
                .rate
                .map(|rate| format!("{rate:.2} Hz"))
                .unwrap_or_else(|| "auto".into());
            let line = format!("{} · {}", first.mode, rate);
            self.label_fit(
                view.rect.x + 10,
                view.rect.y + 29,
                &line,
                self.palette.muted,
                bg,
                11.0,
                f32::from(view.rect.width).max(1.0) - 20.0,
            )?;
        }
        if view.rect.height >= 66 {
            let mut details = first.rotation.clone();
            if view.primary {
                details.push_str(" · primary");
            }
            if view.members.len() > 1 {
                details.push_str(" · mirrored");
            }
            self.label_fit(
                view.rect.x + 10,
                view.rect.y + 44,
                &details,
                self.palette.muted,
                bg,
                10.0,
                f32::from(view.rect.width).max(1.0) - 20.0,
            )?;
        }
        if view.rect.height >= 84 {
            let identity = Self::identity_text(state, &first.name);
            if !identity.is_empty() {
                self.label_fit(
                    view.rect.x + 10,
                    view.rect.y + 60,
                    &identity,
                    self.palette.muted,
                    bg,
                    10.0,
                    f32::from(view.rect.width).max(1.0) - 20.0,
                )?;
            }
        }
        Ok(())
    }

    fn paint_panel(&self, state: &State, ui: &Ui) -> Result<(), String> {
        self.fill(self.palette.surface, ui.l.panel)?;
        self.outline(ui.l.panel, 1, self.palette.border)?;
        self.label(
            ui.l.panel.x + 10,
            ui.l.panel.y + 6,
            "Screens",
            self.palette.muted,
            12.0,
        )?;
        let panel = &ui.panel;
        if panel.total > 0 {
            let page = format!(
                "{}-{} / {}",
                panel.scroll + 1,
                panel.scroll + panel.visible,
                panel.total
            );
            self.label(
                ui.l.panel.x + 70,
                ui.l.panel.y + 7,
                &page,
                self.palette.muted,
                10.0,
            )?;
        }
        if let Some(rect) = panel.page_prev {
            self.button(rect, "<", false, true, self.palette.surface)?;
        }
        if let Some(rect) = panel.page_next {
            self.button(rect, ">", false, true, self.palette.surface)?;
        }
        for (index, rect) in &panel.rows {
            let Some(output) = state.draft.get(*index) else {
                continue;
            };
            let active = *index == state.selected;
            let bg = if active {
                theme::Palette::blend(self.palette.surface, self.palette.accent, 0.18)
            } else {
                self.palette.background
            };
            self.fill(bg, *rect)?;
            if active {
                self.fill(
                    self.palette.accent,
                    Rectangle {
                        width: 2,
                        height: rect.height,
                        ..*rect
                    },
                )?;
            }
            let number = (index + 1).to_string();
            self.badge(
                Rectangle {
                    x: rect.x + 6,
                    y: rect.y + 5,
                    width: 20,
                    height: 17,
                },
                &number,
                if active {
                    self.palette.accent
                } else {
                    self.palette.surface
                },
                if active {
                    self.palette.background
                } else {
                    self.palette.foreground
                },
            )?;
            let text_x = rect.x + 32;
            let max_width = f32::from(rect.width).max(1.0) - 42.0;
            let name = if output.primary {
                format!("{}  primary", output.name)
            } else {
                output.name.clone()
            };
            self.label_fit(
                text_x,
                rect.y + 2,
                &name,
                if output.enabled {
                    self.palette.foreground
                } else {
                    self.palette.muted
                },
                bg,
                12.0,
                max_width,
            )?;
            let detail = if output.enabled {
                let rate = output
                    .rate
                    .map(|rate| format!("{rate:.2} Hz"))
                    .unwrap_or_else(|| "auto".into());
                format!("{} · {}", output.mode, rate)
            } else {
                "disabled".into()
            };
            self.label_fit(
                text_x,
                rect.y + 15,
                &detail,
                self.palette.muted,
                bg,
                10.0,
                max_width,
            )?;
        }
        let selected = state.current();
        let enabled = selected.map(|output| output.enabled).unwrap_or(false);
        let primary = selected.map(|output| output.primary).unwrap_or(false);
        let usable = |hit: Hit| disabled_reason(state, hit).is_none();
        self.button(
            panel.primary,
            if primary {
                "Primary: yes"
            } else {
                "Set primary"
            },
            primary,
            usable(Hit::Primary),
            self.palette.surface,
        )?;
        self.button(
            panel.enabled,
            if enabled { "Enabled: yes" } else { "Disabled" },
            enabled,
            usable(Hit::Enabled),
            self.palette.surface,
        )?;
        self.button(
            panel.rotate,
            &format!(
                "Rotate: {}{}",
                selected
                    .map(|output| output.rotation.as_str())
                    .unwrap_or("normal"),
                if self.font.has('\u{25be}') {
                    " \u{25be}"
                } else {
                    " v"
                }
            ),
            state.popover.as_ref().map(|p| p.kind) == Some(PopKind::Rotation),
            usable(Hit::Rotate),
            self.palette.surface,
        )?;
        self.button(
            panel.identify,
            "Identify",
            false,
            usable(Hit::Identify),
            self.palette.surface,
        )?;
        let arrow = if self.font.has('\u{25be}') {
            " \u{25be}"
        } else {
            " v"
        };
        self.button(
            panel.mode,
            &format!(
                "{}{arrow}",
                selected
                    .filter(|output| output.enabled)
                    .map(|output| output.mode.as_str())
                    .unwrap_or("Mode")
            ),
            state.popover.as_ref().map(|p| p.kind) == Some(PopKind::Mode),
            usable(Hit::Mode),
            self.palette.surface,
        )?;
        self.button(
            panel.rate,
            &format!(
                "{}{arrow}",
                selected
                    .filter(|output| output.enabled)
                    .and_then(|output| output.rate)
                    .map(|rate| format!("{rate:.2} Hz"))
                    .unwrap_or_else(|| "Rate".into())
            ),
            state.popover.as_ref().map(|p| p.kind) == Some(PopKind::Rate),
            usable(Hit::Rate),
            self.palette.surface,
        )?;
        self.button(
            panel.single,
            "Single",
            false,
            usable(Hit::Single),
            self.palette.surface,
        )?;
        self.button(
            panel.extend,
            "Extend",
            false,
            usable(Hit::Extend),
            self.palette.surface,
        )?;
        self.button(
            panel.mirror,
            "Mirror",
            false,
            usable(Hit::Mirror),
            self.palette.surface,
        )?;
        if let (Some(left), true) = (
            state.preview_seconds_left(Instant::now()),
            state.preview_active(),
        ) {
            // U2: a large countdown and a bar that empties.
            self.label_fit(
                panel.info.x,
                panel.info.y - 2,
                &format!("Keep within {left} s"),
                self.palette.accent,
                self.palette.surface,
                18.0,
                f32::from(panel.info.width),
            )?;
            let bar = Rectangle {
                x: panel.info.x,
                y: panel.info.y + 30,
                width: panel.info.width,
                height: 6,
            };
            self.fill(self.palette.border, bar)?;
            let fraction = state.preview_fraction(Instant::now());
            self.fill(
                self.palette.accent,
                Rectangle {
                    width: (f32::from(bar.width) * fraction).round() as u16,
                    ..bar
                },
            )?;
        } else if let Some(output) = selected {
            let rate = output
                .rate
                .map(|rate| format!("{rate:.2} Hz"))
                .unwrap_or_else(|| "auto".into());
            let name = state
                .draft
                .iter()
                .position(|candidate| candidate.name == output.name)
                .map(|index| index + 1)
                .unwrap_or(0);
            let first = format!("Selected {name}: {}", output.name);
            let second = format!(
                "{} × {} · {} · {}",
                output.width(),
                output.height(),
                rate,
                output.rotation
            );
            let detail = Self::identity_detail(state, &output.name);
            self.label_fit(
                panel.info.x,
                panel.info.y,
                &first,
                self.palette.foreground,
                self.palette.surface,
                12.0,
                f32::from(panel.info.width),
            )?;
            self.label_fit(
                panel.info.x,
                panel.info.y + 15,
                &second,
                self.palette.muted,
                self.palette.surface,
                10.0,
                f32::from(panel.info.width),
            )?;
            if !detail.is_empty() {
                self.label_fit(
                    panel.info.x,
                    panel.info.y + 28,
                    &detail,
                    self.palette.muted,
                    self.palette.surface,
                    10.0,
                    f32::from(panel.info.width),
                )?;
            }
        }
        self.button(
            panel.apply[0],
            "Preview",
            false,
            usable(Hit::Apply),
            self.palette.surface,
        )?;
        self.button(
            panel.apply[1],
            &format!(
                "Keep{}",
                state
                    .preview_seconds_left(Instant::now())
                    .map(|left| format!(" ({left}s)"))
                    .unwrap_or_default()
            ),
            state.preview_active(),
            usable(Hit::Keep),
            self.palette.surface,
        )?;
        self.button(
            panel.apply[2],
            "Revert",
            false,
            usable(Hit::Revert),
            self.palette.surface,
        )?;
        Ok(())
    }

    fn paint_profiles(&self, state: &State, ui: &Ui) -> Result<(), String> {
        self.fill(self.palette.surface, ui.l.profiles)?;
        self.outline(ui.l.profiles, 1, self.palette.border)?;
        self.label(
            ui.l.profiles.x + 8,
            ui.l.profiles.y + 6,
            "Profiles",
            self.palette.muted,
            12.0,
        )?;
        let layout = &ui.profiles;
        if layout.pages > 1 {
            self.label(
                ui.l.profiles.x + 78,
                ui.l.profiles.y + 7,
                &format!("{}/{}", layout.page + 1, layout.pages),
                self.palette.muted,
                10.0,
            )?;
        }
        if let Some(rect) = layout.prev {
            self.button(rect, "<", false, true, self.palette.surface)?;
        }
        if let Some(rect) = layout.next {
            self.button(rect, ">", false, true, self.palette.surface)?;
        }
        if let Some(rect) = layout.automatic {
            let on = state.automatic_enabled();
            self.button(
                rect,
                if on {
                    "Automatic: on"
                } else {
                    "Automatic: off"
                },
                on,
                disabled_reason(state, Hit::Automatic).is_none(),
                self.palette.surface,
            )?;
            // Between the title (and page count) and the toggle: what the
            // automation is doing, or an offer to keep its layout.
            let left = ui.l.profiles.x + if layout.pages > 1 { 112 } else { 78 };
            let width = f32::from((rect.x - 8 - left).max(0) as u16);
            let notice = if let Some(screens) = state.automatic_layout() {
                Some((
                    format!("Automatic layout ({screens}) \u{2014} save as profile?"),
                    self.palette.foreground,
                ))
            } else {
                state.status.automatic.as_ref().and_then(|auto| {
                    if !auto.ambiguous.is_empty() {
                        Some((
                            format!("{} fit these screens: pick one", auto.ambiguous.join(", ")),
                            self.palette.warning(),
                        ))
                    } else if auto.retry_in.is_some() {
                        Some((
                            "A screen refused its saved mode; retrying".to_string(),
                            self.palette.warning(),
                        ))
                    } else if auto.enabled && auto.lid.as_deref() == Some("closed") {
                        Some(("Lid closed".to_string(), self.palette.muted))
                    } else {
                        None
                    }
                })
            };
            if let Some((text, colour)) = notice {
                if width > 40.0 {
                    self.label_fit(
                        left,
                        ui.l.profiles.y + 7,
                        &text,
                        colour,
                        self.palette.surface,
                        10.0,
                        width,
                    )?;
                }
            }
        }
        if state.status.profiles.is_empty() {
            self.label(
                ui.l.profiles.x + 8,
                ui.l.profiles.y + 36,
                "No saved profiles yet. Move the screens, then use Save as….",
                self.palette.muted,
                11.0,
            )?;
        }
        for (index, rect) in &layout.rows {
            let Some(profile) = state.status.profiles.get(*index) else {
                continue;
            };
            let info = profile_info(&state.status, profile);
            let selected = state.selected_profile == Some(*index);
            let hovered = self.hover == Some(Hit::Profile(*index));
            self.paint_profile_row(*rect, &profile.name, &info, selected, hovered)?;
        }
        let startup_selected = state
            .selected_profile
            .and_then(|index| state.status.profiles.get(index))
            .map(|profile| state.status.startup_profile.as_deref() == Some(profile.name.as_str()))
            .unwrap_or(false);
        let startup_text = if startup_selected {
            "At login: yes"
        } else {
            "Use at login"
        };
        for (index, text, active) in [
            (0, "Save as…", state.automatic_layout().is_some()),
            (1, "Rename", false),
            (2, "Delete", false),
            (3, startup_text, startup_selected),
            (4, "Reload", false),
        ] {
            let enabled = disabled_reason(
                state,
                [
                    Hit::Save,
                    Hit::Rename,
                    Hit::Delete,
                    Hit::Startup,
                    Hit::Reload,
                ][index],
            )
            .is_none();
            self.button(
                layout.actions[index],
                text,
                active && enabled,
                enabled,
                self.palette.surface,
            )?;
        }
        Ok(())
    }

    /// U3/D10: one profile row. The name and its badges ("current",
    /// "at login") on the first line, the summary or what is missing on the
    /// second. A profile that cannot be used here is dimmed.
    fn paint_profile_row(
        &self,
        rect: Rectangle,
        name: &str,
        info: &ProfileInfo,
        selected: bool,
        hovered: bool,
    ) -> Result<(), String> {
        let usable = info.missing.is_empty();
        let bg = if selected {
            theme::Palette::blend(self.palette.surface, self.palette.accent, 0.28)
        } else if hovered {
            theme::Palette::blend(self.palette.background, self.palette.foreground, 0.07)
        } else {
            self.palette.background
        };
        self.fill(bg, rect)?;
        self.outline(
            rect,
            1,
            if selected {
                self.palette.accent
            } else {
                self.palette.border
            },
        )?;
        if selected {
            self.fill(self.palette.accent, Rectangle { width: 2, ..rect })?;
        }
        // Badges from the right.
        let mut right = rect.x + i16::try_from(rect.width).unwrap_or(0) - 6;
        let mut badges: Vec<(&str, u32, u32)> = Vec::new();
        if info.manual_only {
            badges.push(("manual", self.palette.background, self.palette.muted));
        }
        if info.startup {
            badges.push(("at login", self.palette.surface, self.palette.foreground));
        }
        if info.auto_applied {
            badges.push((
                "auto",
                theme::Palette::blend(self.palette.background, self.palette.accent, 0.45),
                self.palette.foreground,
            ));
        }
        if info.current {
            badges.push(("current", self.palette.accent, self.palette.background));
        }
        for (text, fill, fg) in badges {
            let width = self.text_width(text, 10.0).ceil() as u16 + 10;
            if i32::from(right) - i32::from(width) < i32::from(rect.x) + 60 {
                break;
            }
            right -= i16::try_from(width).unwrap_or(0);
            let badge = Rectangle {
                x: right,
                y: rect.y + 3,
                width,
                height: 13,
            };
            self.fill(fill, badge)?;
            self.label_fit(badge.x + 5, badge.y, text, fg, fill, 10.0, f32::from(width))?;
            right -= 4;
        }
        let text_x = rect.x + 8;
        self.label_fit(
            text_x,
            rect.y + 1,
            name,
            if usable {
                self.palette.foreground
            } else {
                self.palette.muted
            },
            bg,
            12.0,
            f32::from(right - text_x - 4),
        )?;
        let (detail, colour) = if usable {
            (info.summary.clone(), self.palette.muted)
        } else {
            (
                info.reason
                    .clone()
                    .unwrap_or_else(|| format!("needs {}", info.missing.join(", "))),
                theme::Palette::blend(bg, self.palette.warning(), 0.75),
            )
        };
        self.label_fit(
            text_x,
            rect.y + 16,
            &detail,
            colour,
            bg,
            10.0,
            f32::from(rect.width).max(1.0) - 16.0,
        )
    }

    fn paint_name(&self, state: &State) -> Result<(), String> {
        let rect = name_rect(self.w, self.h);
        self.fill(self.palette.background, rect)?;
        self.outline(rect, 2, self.palette.accent)?;
        let title = match state.name_mode {
            Some(NameAction::Save) => "Save display profile as",
            Some(NameAction::Rename) => "Rename display profile",
            None => "",
        };
        self.label(
            rect.x + 16,
            rect.y + 14,
            title,
            self.palette.foreground,
            14.0,
        )?;
        let field = Rectangle {
            x: rect.x + 16,
            y: rect.y + 46,
            width: rect.width.saturating_sub(32),
            height: 30,
        };
        self.button(
            field,
            &format!("{}_", state.name_text),
            true,
            true,
            self.palette.surface,
        )?;
        let name = state.name_text.trim();
        let replace = state.name_mode == Some(NameAction::Save) && state.save_needs_confirm(name);
        let update = state.name_mode == Some(NameAction::Save)
            && !name.is_empty()
            && state.loaded_profile_name() == Some(name);
        let exists = replace;
        let hint = if replace {
            "A profile with this name exists · Enter asks before replacing it · Esc cancels"
        } else if update {
            "Enter updates this profile · Esc cancels"
        } else {
            "Enter confirms · Esc cancels · spaces and Swedish letters are allowed"
        };
        self.label_fit(
            rect.x + 16,
            rect.y + 86,
            hint,
            if exists {
                self.palette.warning()
            } else {
                self.palette.muted
            },
            self.palette.background,
            11.0,
            f32::from(rect.width) - 32.0,
        )?;
        Ok(())
    }

    fn paint_confirm(&self, state: &State) -> Result<(), String> {
        let Some(confirm) = &state.confirm else {
            return Ok(());
        };
        let rect = confirm_rect(self.w, self.h);
        self.fill(self.palette.background, rect)?;
        self.outline(rect, 2, self.palette.error())?;
        let (title, body) = match confirm {
            Confirm::Close => (
                "Close with unsaved changes?".to_string(),
                "The draft has not been previewed and kept.".to_string(),
            ),
            Confirm::Delete(name) => ("Delete display profile?".to_string(), name.clone()),
            Confirm::Replace(name) => (
                format!("Replace profile \u{2039}{name}\u{203a}?"),
                "Its saved layout is overwritten with this draft.".to_string(),
            ),
            Confirm::ClosePreview => (
                "Keep the previewed layout?".to_string(),
                if state.preview_changed() {
                    "A preview is running. Draft changes made after it are not saved.".to_string()
                } else {
                    "A preview is running. Revert restores the previous layout.".to_string()
                },
            ),
        };
        self.label_fit(
            rect.x + 16,
            rect.y + 14,
            &title,
            self.palette.foreground,
            self.palette.background,
            14.0,
            f32::from(rect.width) - 32.0,
        )?;
        self.label_fit(
            rect.x + 16,
            rect.y + 46,
            &body,
            self.palette.muted,
            self.palette.background,
            12.0,
            f32::from(rect.width) - 32.0,
        )?;
        let labels = confirm.buttons();
        for (index, (button, label)) in confirm_buttons(self.w, self.h, labels.len())
            .into_iter()
            .zip(labels)
            .enumerate()
        {
            self.button(button, label, index == 0, true, self.palette.surface)?;
        }
        self.label(
            rect.x + 16,
            rect.y + i16::try_from(rect.height).unwrap_or(0) - 64,
            if matches!(confirm, Confirm::ClosePreview) {
                "Enter keeps · R reverts · Esc cancels"
            } else {
                "Enter confirms · Esc cancels"
            },
            self.palette.muted,
            11.0,
        )?;
        Ok(())
    }

    fn paint_help(&self) -> Result<(), String> {
        let compact = self.w < MIN_WIDTH || self.h < MIN_HEIGHT;
        let (x, y) = if compact { (PAD, PAD) } else { (40, 80) };
        let rect = Rectangle {
            x,
            y,
            width: self.w.saturating_sub(x as u16 * 2),
            height: self.h.saturating_sub(y as u16 * 2),
        };
        self.fill(self.palette.background, rect)?;
        self.outline(rect, 2, self.palette.accent)?;
        self.label(
            rect.x + 20,
            rect.y + 16,
            "NIRU Display",
            self.palette.foreground,
            20.0,
        )?;
        let lines = [
            "Mouse        drag a screen; edges snap to shared borders",
            "Tab          move between every control",
            "Arrows       move the selected screen; Shift moves 10 px",
            "Enter/Space  press the focused control; Enter alone previews",
            "I            identify every screen with its number and model",
            "R            reload the actual display state",
            "Ctrl+Z       undo the last draft change",
            "F1           open or close this help",
            "Esc/Q        close; during a preview it asks: Keep layout or Revert",
            "Profiles     click to load, double-click to preview; Save as… stores the draft",
            "Preview      changes stay for 15 s; Keep confirms, Revert restores",
            "Screens      click the < > buttons or use the wheel to page",
            "Canvas       wheel zooms; middle-drag pans; 0 resets the view",
            "Mode/Rate    click or Enter lists the choices; left/right step through them",
        ];
        let spacing = ((i32::from(rect.height) - 70) / lines.len() as i32).clamp(14, 22) as i16;
        for (index, line) in lines.iter().enumerate() {
            self.label_fit(
                rect.x + 20,
                rect.y + 58 + index as i16 * spacing,
                line,
                self.palette.muted,
                self.palette.background,
                12.0,
                f32::from(rect.width) - 40.0,
            )?;
        }
        Ok(())
    }

    fn paint_footer(&self, state: &State, ui: &Ui, help: bool) -> Result<(), String> {
        let issues = validate_draft(&state.draft);
        let blocking = issues.iter().find(|issue| issue.blocking);
        let (message, colour) = if state.busy {
            (state.busy_text.to_string(), self.palette.muted)
        } else if state.message_is_error {
            (state.message.clone(), self.palette.error())
        } else if let Some(issue) = blocking {
            (issue.text.clone(), self.palette.error())
        } else {
            (state.message.clone(), self.palette.muted)
        };
        let mut lines = self.wrap_text(&message, 12.0, f32::from(ui.l.footer.width));
        if lines.len() > 2 {
            lines.truncate(2);
            if let Some(last) = lines.last_mut() {
                *last = self.fit_text(last, 12.0, f32::from(ui.l.footer.width));
            }
        }
        for (index, line) in lines.iter().enumerate() {
            self.label_fit(
                ui.l.footer.x,
                ui.l.footer.y + index as i16 * 14,
                line,
                colour,
                self.palette.background,
                12.0,
                f32::from(ui.l.footer.width),
            )?;
        }
        if !help && state.name_mode.is_none() && state.confirm.is_none() {
            self.label_fit(
                ui.l.footer.x,
                ui.l.footer.y + 32,
                if state.own_preview && state.preview_active() {
                    "Enter keeps the layout · Esc reverts · Tab focus · F1 help"
                } else {
                    "Tab focus · drag screens · arrows move · I identify · Ctrl+Z undo · Enter preview · F1 help · Esc close"
                },
                self.palette.muted,
                self.palette.background,
                10.0,
                f32::from(ui.l.footer.width),
            )?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), String> {
        self.c
            .copy_area(self.back, self.win, self.gc, 0, 0, 0, 0, self.w, self.h)
            .map_err(|e| e.to_string())?;
        self.c.flush().map_err(|e| e.to_string())
    }
}

fn load_cursors(c: &RustConnection) -> Option<Cursors> {
    let font = c.generate_id().ok()?;
    c.open_font(font, b"cursor").ok()?.check().ok()?;
    let make = |glyph: u16| -> Option<Cursor> {
        let cursor = c.generate_id().ok()?;
        c.create_glyph_cursor(
            cursor, font, font, glyph, glyph, 0, 0, 0, 0xffff, 0xffff, 0xffff,
        )
        .ok()?
        .check()
        .ok()?;
        Some(cursor)
    };
    Some(Cursors {
        normal: make(68)?,
        fleur: make(52)?,
        hand: make(60)?,
    })
}

/// Wait until the X socket, the worker's wake pipe or the file watcher has
/// something, or until `timeout` (`None`: no timeout at all).
fn wait_for_input(fds: &[libc::c_int], timeout: Option<Duration>) {
    let mut polls: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let ms = timeout
        .map(|t| t.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32)
        .unwrap_or(-1);
    // SAFETY: a live array of pollfd structures and its length.
    let _ = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, ms) };
}

/// How often a running preview's status is read again, to notice a Keep or
/// Revert made elsewhere when the layout itself does not change.
const PREVIEW_RECHECK: Duration = Duration::from_secs(5);
/// How long after the local preview deadline the window looks again, so the
/// watchdog has had time to restore the previous layout.
const PREVIEW_GRACE: Duration = Duration::from_millis(700);

/// The event loop's bookkeeping outside the window and the draft.
struct Loop {
    help: bool,
    dirty: bool,
    /// RandR events, debounced into one stamp check.
    randr: watch::Debounce,
    /// A debounced RandR event waits for the stamp check.
    randr_check: bool,
    /// inotify events on the profile or preview file, debounced.
    files: watch::Debounce,
    /// The display files as the last status read saw them.
    file_stamps: [watch::FileStamp; 2],
    /// A status read that does not depend on the RandR stamps is due.
    reload_due: bool,
    /// A background status read is on the worker.
    background: bool,
    /// The palette must be read again before the next paint.
    theme: bool,
    last_preview_read: Instant,
    expiry_checked: bool,
    next_tick: Option<Instant>,
}

impl Loop {
    fn new() -> Self {
        Self {
            help: false,
            dirty: true,
            randr: watch::Debounce::default(),
            randr_check: false,
            files: watch::Debounce::default(),
            file_stamps: display_file_stamps(),
            reload_due: false,
            background: false,
            theme: false,
            last_preview_read: Instant::now(),
            expiry_checked: false,
            next_tick: None,
        }
    }

    /// How long the loop may sleep: until the next debounce, the identify
    /// overlay's end or the preview's next second, or forever.
    fn timeout(&self, app: &App, state: &State, now: Instant) -> Option<Duration> {
        let mut at: Vec<Instant> = Vec::new();
        for debounce in [&self.randr, &self.files] {
            if let Some(left) = debounce.timeout(now) {
                at.push(now + left);
            }
        }
        if let Some(until) = app.identify_until {
            at.push(until);
        }
        if state.preview_active() {
            if let Some(tick) = self.next_tick {
                at.push(tick);
            }
            at.push(self.last_preview_read + PREVIEW_RECHECK);
            if let Some(deadline) = state.preview_deadline() {
                if !self.expiry_checked {
                    at.push(deadline + PREVIEW_GRACE);
                }
            }
        }
        at.into_iter()
            .min()
            .map(|when| when.saturating_duration_since(now))
    }
}

fn display_file_stamps() -> [watch::FileStamp; 2] {
    watch::display_files().map(|path| watch::file_stamp(&path))
}

/// The time of the preview countdown's next whole second.
fn next_second(deadline: Instant, now: Instant) -> Instant {
    let left = deadline.saturating_duration_since(now);
    let fraction = Duration::from_nanos((left.as_nanos() % 1_000_000_000) as u64);
    let step = if fraction.is_zero() {
        Duration::from_secs(1)
    } else {
        fraction
    };
    now + step
}

pub fn run(app: &mut App, state: &mut State) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let wake = std::sync::Arc::new(watch::Wake::new()?);
    let mut worker: backend::Worker<After> = {
        let wake = wake.clone();
        backend::Worker::start(move || wake.notify())
    };
    let mut watcher = watch::Watcher::new().ok();
    if let Some(watcher) = watcher.as_mut() {
        watcher.set(watch::watched_dirs());
    }
    let mut guard = watch::StampGuard::default();
    guard.record(app.stamps());
    let mut lp = Loop::new();
    let mut last_theme = Instant::now();
    let mut pending: Option<Event> = None;
    loop {
        // 1. Every X event that is queued.
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => match app.c.poll_for_event().map_err(|e| e.to_string())? {
                    Some(event) => event,
                    None => break,
                },
            };
            handle_event(app, state, &mut lp, event)?;
        }
        // 2. Answers from the worker.
        wake.drain();
        while let Some(done) = worker.try_recv() {
            if done.after.background() {
                lp.background = false;
            }
            let read = done.status.is_ok();
            state.finish(done);
            if read {
                // Recorded after the read, so the reprobe that niructl's
                // xrandr call made cannot look like a change.
                guard.record(app.stamps());
                lp.file_stamps = display_file_stamps();
                lp.last_preview_read = Instant::now();
                lp.expiry_checked = false;
            }
            lp.dirty = true;
        }
        // 3. Files niructl writes, and the theme.
        if let Some(watcher) = watcher.as_mut() {
            let changes = watcher.drain();
            if changes.theme {
                lp.theme = true;
                lp.dirty = true;
            }
            if changes.display {
                lp.files.note(Instant::now());
            }
            watcher.set(watch::watched_dirs());
        }
        // 4. Debounces and the preview clock.
        let now = Instant::now();
        if lp.randr.take(now) {
            lp.randr_check = true;
        }
        if lp.files.take(now) && display_file_stamps() != lp.file_stamps {
            lp.reload_due = true;
        }
        if state.preview_active() {
            if now >= lp.last_preview_read + PREVIEW_RECHECK {
                lp.reload_due = true;
            }
            if let Some(deadline) = state.preview_deadline() {
                if !lp.expiry_checked && now >= deadline + PREVIEW_GRACE {
                    lp.expiry_checked = true;
                    lp.reload_due = true;
                }
                if lp.next_tick.is_none_or(|tick| now >= tick) {
                    lp.next_tick = Some(next_second(deadline, now));
                    lp.dirty = true;
                }
            }
        } else {
            lp.next_tick = None;
        }
        // 5. One background status read at a time, never during a drag
        // (the drag holds output indices) and never behind an action, whose
        // own status read answers the same question.
        if !lp.background && !state.busy && state.outbox.is_empty() && app.drag.is_none() {
            let randr_changed = lp.randr_check && guard.changed(app.stamps());
            lp.randr_check = false;
            if lp.reload_due || randr_changed {
                lp.reload_due = false;
                lp.background = true;
                lp.last_preview_read = now;
                state.submit(
                    backend::Job::Status,
                    After::Reload {
                        forced: false,
                        background: true,
                    },
                );
            }
        }
        for (job, after) in state.outbox.drain(..) {
            worker.submit(job, after);
        }
        if state.quit {
            break;
        }
        if let Some(until) = app.identify_until {
            if Instant::now() >= until {
                let _ = app.clear_identify();
                lp.dirty = true;
            }
        }
        // 6. Paint.
        if lp.dirty {
            // Without inotify the palette is read again on repaints, at most
            // every 750 ms, as before.
            if lp.theme || (watcher.is_none() && last_theme.elapsed() >= Duration::from_millis(750))
            {
                app.palette = theme::load();
                last_theme = Instant::now();
                lp.theme = false;
            }
            let ui = app.ui(state);
            app.paint(state, &ui, lp.help)?;
            app.flush()?;
            lp.dirty = false;
        } else {
            app.c.flush().map_err(|e| e.to_string())?;
        }
        // 7. Sleep until something happens. Replies read while painting may
        // have queued events that poll(2) would not see.
        if let Some(event) = app.c.poll_for_event().map_err(|e| e.to_string())? {
            pending = Some(event);
            continue;
        }
        let mut fds = vec![app.c.stream().as_raw_fd(), wake.fd()];
        if let Some(watcher) = watcher.as_ref() {
            fds.push(watcher.fd());
        }
        wait_for_input(&fds, lp.timeout(app, state, Instant::now()));
    }
    Ok(())
}

fn handle_event(
    app: &mut App,
    state: &mut State,
    lp: &mut Loop,
    event: Event,
) -> Result<(), String> {
    if !matches!(
        event,
        Event::NoExposure(_)
            // A restack (a lock screen or a panel raising itself) sends
            // ConfigureNotify without a new size: nothing to repaint.
            | Event::ConfigureNotify(_)
            | Event::MotionNotify(_)
            | Event::MappingNotify(_)
            | Event::RandrScreenChangeNotify(_)
            | Event::RandrNotify(_)
    ) {
        lp.dirty = true;
    }
    match event {
        Event::Expose(e) if e.count != 0 => {}
        Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
            // A hotplug, a mode change or a property update. The stamp
            // check after the debounce decides whether it is news.
            lp.randr.note(Instant::now());
        }
        Event::ConfigureNotify(e) => {
            // D6: paint for the real size, even when a tiling layout makes
            // the window smaller than its minimum.
            if e.width != app.w || e.height != app.h {
                app.w = e.width.max(1);
                app.h = e.height.max(1);
                app.resize_backing()?;
                app.scroll_by(0);
                lp.dirty = true;
            }
        }
        Event::MappingNotify(_) => {
            // setxkbmap and every other keymap change: read the whole
            // table again so Shift and AltGr stay right.
            if let Ok(keys) = keyboard::Keys::load(&app.c) {
                app.keys = keys;
            }
        }
        Event::KeyPress(e) => {
            // Shortcuts compare against the key's base symbol, text against
            // the symbol of the active level.
            let key = match app.keys.base(e.detail) {
                K_KP_ENTER => K_RETURN,
                key => key,
            };
            let typed = keyboard::text_char(app.keys.sym(e.detail, e.state));
            let shift = e.state.contains(KeyButMask::SHIFT);
            let ctrl = e.state.contains(KeyButMask::CONTROL);
            if state.busy {
                // While an action runs only Esc is accepted: it closes the
                // window at once. niructl finishes the action on its own,
                // and a preview stays under its watchdog.
                if key == K_ESCAPE {
                    state.quit = true;
                }
                return Ok(());
            }
            if state.name_mode.is_some() {
                match key {
                    K_ESCAPE => {
                        state.name_mode = None;
                        state.name_text.clear();
                        state.focus = None;
                    }
                    K_BACKSPACE => {
                        state.name_text.pop();
                    }
                    K_RETURN => {
                        let action = state.name_mode.take();
                        let text = state.name_text.trim().to_string();
                        state.name_text.clear();
                        state.focus = None;
                        let result = match action {
                            Some(NameAction::Save) if state.save_needs_confirm(&text) => {
                                state.confirm = Some(Confirm::Replace(text));
                                Ok(())
                            }
                            Some(NameAction::Save) => state.save(&text),
                            Some(NameAction::Rename) => state.rename(&text),
                            None => Ok(()),
                        };
                        if let Err(err) = result {
                            state.set_message(err, true);
                        }
                    }
                    _ => {
                        if let Some(ch) = typed {
                            if !ctrl && state.name_text.chars().count() < 64 {
                                state.name_text.push(ch);
                            }
                        }
                    }
                }
                return Ok(());
            }
            if let Some(confirm) = &state.confirm {
                let preview = matches!(confirm, Confirm::ClosePreview);
                match key {
                    K_ESCAPE | K_N => {
                        state.confirm = None;
                    }
                    K_RETURN | K_Y => confirm_action(state, 0),
                    K_K if preview => confirm_action(state, 0),
                    K_R if preview => confirm_action(state, 1),
                    _ => {}
                }
                return Ok(());
            }
            if lp.help {
                lp.help = false;
                return Ok(());
            }
            if state.popover.is_some() {
                state.popover_key(key);
                return Ok(());
            }
            if layout(app.w, app.h).too_small && !matches!(key, K_ESCAPE | K_Q) {
                // Nothing is drawn to act on; only closing works.
                return Ok(());
            }
            if key == K_ESCAPE {
                if app.drag.is_some() {
                    cancel_drag(app, state);
                } else if state.focus.is_some() {
                    state.focus = None;
                } else if state.own_preview && state.preview_active() {
                    // U2: Esc reverts a preview this window started.
                    if let Err(err) = state.revert() {
                        state.set_message(err, true);
                    }
                } else {
                    request_close(state);
                }
                return Ok(());
            }
            if key == K_Q {
                request_close(state);
                return Ok(());
            }
            if key == K_F1 {
                lp.help = true;
                return Ok(());
            }
            if ctrl && key == K_Z {
                state.undo();
                return Ok(());
            }
            if ctrl && key == K_S {
                let ui = app.ui(state);
                activate(app, state, &ui, Hit::Save)?;
                return Ok(());
            }
            if key == K_I && !ctrl {
                if let Err(err) = app.identify(state) {
                    state.set_message(err, true);
                }
                return Ok(());
            }
            if key == K_R && !ctrl {
                state.request_reload(true);
                return Ok(());
            }
            if key == K_0 && !ctrl {
                app.zoom = 1.0;
                app.pan = (0.0, 0.0);
                state.set_message("Canvas zoom reset.", false);
                return Ok(());
            }
            if key == K_TAB {
                let ui = app.ui(state);
                let order = focus_order(&ui);
                if order.is_empty() {
                    return Ok(());
                }
                let current = state
                    .focus
                    .and_then(|focus| order.iter().position(|hit| *hit == focus));
                let next = match current {
                    Some(index) if shift => (index + order.len() - 1) % order.len(),
                    Some(index) => (index + 1) % order.len(),
                    None if shift => order.len().saturating_sub(1),
                    None => 0,
                };
                state.focus = order.get(next).copied();
                app.reveal(state);
                return Ok(());
            }
            if matches!(key, K_PAGEUP | K_PAGEDOWN) {
                let ui = app.ui(state);
                page_outputs(state, &ui, if key == K_PAGEUP { -1 } else { 1 });
                return Ok(());
            }
            if matches!(key, K_LEFT | K_RIGHT | K_UP | K_DOWN) {
                let step = if shift { 10 } else { 1 };
                let ui = app.ui(state);
                handle_arrows(state, &ui, key, step);
                return Ok(());
            }
            if key == K_RETURN || key == K_SPACE {
                if let Some(focus) = state.focus {
                    let ui = app.ui(state);
                    activate(app, state, &ui, focus)?;
                } else if key == K_RETURN {
                    // U2: Enter keeps a preview this window started.
                    let result = if state.own_preview && state.preview_active() {
                        state.keep()
                    } else {
                        state.apply_preview()
                    };
                    if let Err(err) = result {
                        state.set_message(err, true);
                    }
                }
                return Ok(());
            }
        }
        Event::ButtonPress(e) => {
            if state.busy || state.name_mode.is_some() {
                return Ok(());
            }
            if let Some(confirm) = &state.confirm {
                let buttons = confirm_buttons(app.w, app.h, confirm.buttons().len());
                if let Some(index) = buttons
                    .iter()
                    .position(|rect| e.detail == 1 && contains(*rect, e.event_x, e.event_y))
                {
                    confirm_action(state, index);
                }
                return Ok(());
            }
            let ui = app.ui(state);
            if state.popover.is_some() {
                // U1: a click on a row chooses it; the wheel scrolls the
                // list; any other click closes it.
                match (e.detail, hit(state, &ui, e.event_x, e.event_y)) {
                    (1, Some(Hit::PopItem(index))) => state.choose_popover(index),
                    (4, _) => {
                        state.popover_key(K_UP);
                    }
                    (5, _) => {
                        state.popover_key(K_DOWN);
                    }
                    _ => state.popover = None,
                }
                return Ok(());
            }
            if e.detail == 4 || e.detail == 5 {
                let up = e.detail == 4;
                let over_canvas = contains(ui.l.canvas, e.event_x, e.event_y);
                if ui.l.max_scroll > 0 && !over_canvas {
                    // D6: in the compact layout the wheel scrolls the content.
                    app.scroll_by(if up { -40 } else { 40 });
                } else if contains(ui.l.profiles, e.event_x, e.event_y) {
                    page_profiles(state, &ui, if e.detail == 4 { -1 } else { 1 });
                } else if contains(ui.l.panel, e.event_x, e.event_y) {
                    page_outputs(state, &ui, if e.detail == 4 { -1 } else { 1 });
                } else if contains(ui.l.canvas, e.event_x, e.event_y) {
                    app.zoom = if e.detail == 4 {
                        (app.zoom * 1.1).min(4.0)
                    } else {
                        (app.zoom / 1.1).max(0.5)
                    };
                    if (app.zoom - 1.0).abs() < 0.001 {
                        app.zoom = 1.0;
                        app.pan = (0.0, 0.0);
                    }
                }
                return Ok(());
            }
            if e.detail == 2 && contains(ui.l.canvas, e.event_x, e.event_y) {
                app.pan_drag = Some((e.event_x, e.event_y, app.pan.0, app.pan.1));
                return Ok(());
            }
            state.focus = None;
            if e.detail != 1 {
                return Ok(());
            }
            if let Some(Hit::Monitor(index)) = hit(state, &ui, e.event_x, e.event_y) {
                state.selected = index;
                let group = state.mirror_group(index);
                let start_positions = group
                    .iter()
                    .filter_map(|candidate| {
                        state
                            .draft
                            .get(*candidate)
                            .map(|output| (*candidate, output.x, output.y))
                    })
                    .collect();
                app.drag = Some(Drag {
                    group,
                    start_mouse: (e.event_x, e.event_y),
                    start_positions,
                    canvas: ui.canvas,
                    moved: false,
                    was_dirty: state.dirty,
                    guide_x: None,
                    guide_y: None,
                });
                return Ok(());
            }
            if let Some(action) = hit(state, &ui, e.event_x, e.event_y) {
                let now = Instant::now();
                let double = app.last_click.is_some_and(|(previous, at)| {
                    previous == action && now.duration_since(at) <= DOUBLE_CLICK
                });
                app.last_click = Some((action, now));
                if let (Hit::Profile(index), true) = (action, double) {
                    // U3: a double-click loads the profile and previews it.
                    app.last_click = None;
                    if state.selected_profile == Some(index) && state.select_profile(index) {
                        if let Err(err) = state.apply_preview() {
                            state.set_message(err, true);
                        }
                    }
                    return Ok(());
                }
                activate(app, state, &ui, action)?;
            }
        }
        Event::ButtonRelease(_) => {
            app.pan_drag = None;
            finish_drag(app, state);
        }
        Event::MotionNotify(e) => {
            if let Some((start_x, start_y, pan_x, pan_y)) = app.pan_drag {
                app.pan = (
                    pan_x + f32::from(e.event_x - start_x),
                    pan_y + f32::from(e.event_y - start_y),
                );
                lp.dirty = true;
                return Ok(());
            }
            if app.drag.is_some() {
                if !e.state.contains(KeyButMask::BUTTON1) {
                    finish_drag(app, state);
                } else {
                    drag_motion(state, app, (e.event_x, e.event_y));
                }
                lp.dirty = true;
                return Ok(());
            }
            let ui = app.ui(state);
            let hover = hit(state, &ui, e.event_x, e.event_y);
            if hover != app.hover {
                app.hover = hover;
                let cursor = app.cursors.map(|cursors| match hover {
                    Some(Hit::Monitor(_)) => cursors.fleur,
                    Some(Hit::NameField) | None => cursors.normal,
                    Some(_) => cursors.hand,
                });
                app.set_cursor(cursor)?;
                lp.dirty = true;
            }
        }
        Event::ClientMessage(e) if is_close_request(&e, app.wm_protocols, app.wm_delete_window) => {
            if state.busy {
                state.quit = true;
            } else {
                request_close(state);
            }
        }
        _ => {}
    }
    Ok(())
}

/// A `WM_PROTOCOLS` client message whose first value is `WM_DELETE_WINDOW`.
fn is_close_request(e: &ClientMessageEvent, protocols: Atom, delete: Atom) -> bool {
    e.type_ == protocols && e.format == 32 && e.data.as_data32()[0] == delete
}

fn request_close(state: &mut State) {
    if state.name_mode.is_some() {
        state.name_mode = None;
        state.name_text.clear();
        return;
    }
    if state.confirm.is_some() {
        return;
    }
    if state.popover.take().is_some() {
        return;
    }
    // D9: closing never reverts a preview without asking. A crash or a
    // kill still reverts through niructl's watchdog.
    if state.preview_active() {
        state.confirm = Some(Confirm::ClosePreview);
    } else if state.dirty {
        state.confirm = Some(Confirm::Close);
    } else {
        state.quit = true;
    }
}

/// Press button `index` of the open confirm dialog; the last is Cancel.
fn confirm_action(state: &mut State, index: usize) {
    let Some(confirm) = state.confirm.take() else {
        return;
    };
    let result = match (confirm, index) {
        (Confirm::Close, 0) => {
            state.quit = true;
            Ok(())
        }
        (Confirm::Delete(name), 0) => state.delete_named(&name),
        (Confirm::Replace(name), 0) => state.save(&name),
        // D9: Keep layout or Revert, then close once niructl has answered.
        (Confirm::ClosePreview, 0) => {
            state.submit(backend::Job::Keep, After::Keep { quit: true });
            Ok(())
        }
        (Confirm::ClosePreview, 1) => {
            state.submit(backend::Job::Revert, After::Revert { quit: true });
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(err) = result {
        state.set_message(err, true);
    }
}

fn page_outputs(state: &mut State, ui: &Ui, direction: i32) {
    let visible = ui.panel.visible.max(1);
    if direction > 0 {
        state.output_scroll =
            (state.output_scroll + visible).min(state.draft.len().saturating_sub(visible));
    } else {
        state.output_scroll = state.output_scroll.saturating_sub(visible);
    }
}

fn page_profiles(state: &mut State, ui: &Ui, direction: i32) {
    let per_page = ui.profiles.per_page.max(1) as i32;
    let pages = ui.profiles.pages as i32;
    let page = (ui.profiles.page as i32 + direction).clamp(0, pages.saturating_sub(1));
    state.profile_scroll = (page * per_page) as usize;
}

fn handle_arrows(state: &mut State, ui: &Ui, key: u32, step: i32) {
    match state.focus {
        // U1 keeps the left/right shortcuts on the focused list buttons.
        Some(Hit::Mode) => {
            let delta = if matches!(key, K_LEFT | K_UP) { -1 } else { 1 };
            state.cycle_mode(delta);
        }
        Some(Hit::Rate) => {
            let delta = if matches!(key, K_LEFT | K_UP) { -1 } else { 1 };
            state.cycle_rate(delta);
        }
        Some(Hit::Rotate) => {
            let delta = if matches!(key, K_LEFT | K_UP) { -1 } else { 1 };
            state.cycle_rotation(delta);
        }
        Some(Hit::Output(_)) => match key {
            K_UP => {
                if state.selected > 0 {
                    state.selected -= 1;
                }
            }
            K_DOWN => {
                if state.selected + 1 < state.draft.len() {
                    state.selected += 1;
                }
            }
            K_LEFT => state.move_selected(-step, 0),
            K_RIGHT => state.move_selected(step, 0),
            _ => {}
        },
        None => match key {
            K_LEFT => state.move_selected(-step, 0),
            K_RIGHT => state.move_selected(step, 0),
            K_UP => state.move_selected(0, -step),
            K_DOWN => state.move_selected(0, step),
            _ => {}
        },
        _ => {}
    }
    let visible = ui.panel.visible.max(1);
    if state.selected < state.output_scroll {
        state.output_scroll = state.selected;
    } else if state.selected >= state.output_scroll + visible {
        state.output_scroll = state.selected + 1 - visible;
    }
}

fn finish_drag(app: &mut App, state: &mut State) {
    let Some(drag) = app.drag.take() else {
        return;
    };
    app.hover = None;
    if drag.moved {
        if normalize_draft(&mut state.draft) {
            state.dirty = true;
        }
        state.set_message("Screen moved. Preview to apply the layout.", false);
    }
}

fn cancel_drag(app: &mut App, state: &mut State) {
    if let Some(drag) = app.drag.take() {
        if drag.moved {
            for (index, start_x, start_y) in drag.start_positions {
                if let Some(output) = state.draft.get_mut(index) {
                    output.x = start_x;
                    output.y = start_y;
                }
            }
            state.dirty = drag.was_dirty;
            if state
                .undo
                .last()
                .map(|draft| draft == &state.draft)
                .unwrap_or(false)
            {
                state.undo.pop();
            }
            state.set_message("Drag cancelled.", false);
        }
    }
    app.hover = None;
}

fn drag_motion(state: &mut State, app: &mut App, mouse: (i16, i16)) {
    let Some(drag) = app.drag.as_mut() else {
        return;
    };
    let raw_dx = mouse.0 - drag.start_mouse.0;
    let raw_dy = mouse.1 - drag.start_mouse.1;
    if !drag.moved && raw_dx.abs() < 4 && raw_dy.abs() < 4 {
        return;
    }
    if !drag.moved {
        state.checkpoint();
        drag.moved = true;
    }
    let dx = (f32::from(raw_dx) / drag.canvas.scale).round() as i32;
    let dy = (f32::from(raw_dy) / drag.canvas.scale).round() as i32;
    let threshold = (8.0 / drag.canvas.scale).max(1.0);
    let (dx, dy, guide_x, guide_y) = snap_group(
        &state.draft,
        &drag.group,
        &drag.start_positions,
        dx,
        dy,
        threshold,
    );
    for (index, start_x, start_y) in &drag.start_positions {
        if let Some(output) = state.draft.get_mut(*index) {
            output.x = start_x.saturating_add(dx);
            output.y = start_y.saturating_add(dy);
        }
    }
    drag.guide_x = guide_x;
    drag.guide_y = guide_y;
    state.dirty = true;
}

fn snap_group(
    draft: &[DraftOutput],
    group: &[usize],
    start: &[(usize, i32, i32)],
    dx: i32,
    dy: i32,
    threshold: f32,
) -> (i32, i32, Option<i32>, Option<i32>) {
    let Some(&(index, start_x, start_y)) = start.first() else {
        return (dx, dy, None, None);
    };
    let Some(source) = draft.get(index) else {
        return (dx, dy, None, None);
    };
    let width = source.width();
    let height = source.height();
    let proposed_x = start_x.saturating_add(dx);
    let proposed_y = start_y.saturating_add(dy);
    let mut best_x: Option<(f32, i32, i32)> = None;
    let mut best_y: Option<(f32, i32, i32)> = None;
    let mut consider_x = |candidate: i32, guide: i32| {
        let distance = (candidate - proposed_x).abs() as f32;
        if distance <= threshold
            && best_x
                .map(|(best_distance, _, _)| distance < best_distance)
                .unwrap_or(true)
        {
            best_x = Some((distance, candidate, guide));
        }
    };
    let mut consider_y = |candidate: i32, guide: i32| {
        let distance = (candidate - proposed_y).abs() as f32;
        if distance <= threshold
            && best_y
                .map(|(best_distance, _, _)| distance < best_distance)
                .unwrap_or(true)
        {
            best_y = Some((distance, candidate, guide));
        }
    };
    consider_x(0, 0);
    consider_y(0, 0);
    for (other_index, other) in draft.iter().enumerate() {
        if group.contains(&other_index) || !other.enabled {
            continue;
        }
        let (other_width, other_height) = (other.width(), other.height());
        for (candidate, guide) in [
            (other.x, other.x),
            (other.x + other_width, other.x + other_width),
            (other.x - width, other.x),
            (other.x + other_width - width, other.x + other_width),
        ] {
            consider_x(candidate, guide);
        }
        for (candidate, guide) in [
            (other.y, other.y),
            (other.y + other_height, other.y + other_height),
            (other.y - height, other.y),
            (other.y + other_height - height, other.y + other_height),
        ] {
            consider_y(candidate, guide);
        }
    }
    let adjusted_x = best_x.map(|(_, target, _)| target).unwrap_or(proposed_x);
    let adjusted_y = best_y.map(|(_, target, _)| target).unwrap_or(proposed_y);
    (
        dx + (adjusted_x - proposed_x),
        dy + (adjusted_y - proposed_y),
        best_x.map(|(_, _, guide)| guide),
        best_y.map(|(_, _, guide)| guide),
    )
}

fn activate(app: &mut App, state: &mut State, ui: &Ui, action: Hit) -> Result<(), String> {
    let result = match action {
        Hit::Monitor(index) | Hit::Output(index) => {
            if index < state.draft.len() {
                state.selected = index;
            }
            Ok(())
        }
        Hit::OutputPrev => {
            page_outputs(state, ui, -1);
            Ok(())
        }
        Hit::OutputNext => {
            page_outputs(state, ui, 1);
            Ok(())
        }
        Hit::Profile(index) => {
            state.select_profile(index);
            Ok(())
        }
        Hit::ProfilePrev => {
            page_profiles(state, ui, -1);
            Ok(())
        }
        Hit::ProfileNext => {
            page_profiles(state, ui, 1);
            Ok(())
        }
        Hit::Enabled => {
            state.toggle_enabled();
            Ok(())
        }
        Hit::Primary => {
            state.set_primary();
            Ok(())
        }
        Hit::Rotate => state.open_popover(PopKind::Rotation),
        Hit::Mode => state.open_popover(PopKind::Mode),
        Hit::Rate => state.open_popover(PopKind::Rate),
        Hit::PopItem(index) => {
            state.choose_popover(index);
            Ok(())
        }
        Hit::Single => {
            state.single_screen();
            Ok(())
        }
        Hit::Extend => {
            state.extend_row();
            Ok(())
        }
        Hit::Mirror => {
            state.mirror_screens();
            Ok(())
        }
        Hit::Identify => app.identify(state),
        Hit::Apply => state.apply_preview(),
        Hit::Keep => state.keep(),
        Hit::Revert => state.revert(),
        Hit::Save => {
            state.name_mode = Some(NameAction::Save);
            // An automatic layout suggests its screens as the name.
            state.name_text = state
                .selected_profile
                .and_then(|index| state.status.profiles.get(index))
                .map(|profile| profile.name.clone())
                .or_else(|| state.automatic_layout().map(str::to_string))
                .unwrap_or_default();
            state.focus = Some(Hit::NameField);
            Ok(())
        }
        Hit::Rename => {
            if let Some(index) = state.selected_profile {
                if let Some(profile) = state.status.profiles.get(index) {
                    state.name_mode = Some(NameAction::Rename);
                    state.name_text = profile.name.clone();
                    state.focus = Some(Hit::NameField);
                }
            } else {
                state.set_message("Select a saved profile first.", true);
            }
            Ok(())
        }
        Hit::Delete => {
            if let Some(index) = state.selected_profile {
                if let Some(profile) = state.status.profiles.get(index) {
                    state.confirm = Some(Confirm::Delete(profile.name.clone()));
                }
            } else {
                state.set_message("Select a saved profile first.", true);
            }
            Ok(())
        }
        Hit::Startup => state.toggle_startup(),
        Hit::Automatic => state.toggle_automatic(),
        Hit::Reload => {
            state.request_reload(true);
            Ok(())
        }
        Hit::NameField => Ok(()),
    };
    if let Err(err) = result {
        state.set_message(err, true);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, x: i32, y: i32, mode: &str) -> DraftOutput {
        DraftOutput {
            name: name.into(),
            connected: true,
            enabled: true,
            mode: mode.into(),
            rate: Some(60.0),
            x,
            y,
            rotation: "normal".into(),
            primary: false,
        }
    }

    fn state_with(draft: Vec<DraftOutput>) -> State {
        let mut state = State::new(backend::Status::default());
        state.draft = draft;
        state
    }

    #[test]
    fn mode_sizes_ignore_interlace_suffixes() {
        assert_eq!(mode_size("2560x1440"), Some((2560, 1440)));
        assert_eq!(mode_size("1920x1080i"), Some((1920, 1080)));
        let mut rotated = output("eDP-1", 0, 0, "1920x1080");
        rotated.rotation = "left".into();
        assert_eq!(rotated.size(), (1080, 1920));
    }

    #[test]
    fn normalization_keeps_the_relative_layout() {
        let mut draft = vec![
            output("a", -100, -50, "1920x1080"),
            output("b", 1820, 0, "1920x1080"),
        ];
        assert!(normalize_draft(&mut draft));
        assert_eq!((draft[0].x, draft[0].y), (0, 0));
        assert_eq!((draft[1].x, draft[1].y), (1920, 50));
    }

    #[test]
    fn mirroring_is_not_an_overlap_but_a_partial_overlap_is() {
        let mirrored = vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 0, 0, "1920x1080"),
        ];
        assert!(validate_draft(&mirrored)
            .iter()
            .all(|issue| !issue.blocking));
        let overlapping = vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1000, 0, "1920x1080"),
        ];
        assert!(validate_draft(&overlapping)
            .iter()
            .any(|issue| issue.blocking));
    }

    #[test]
    fn snap_moves_the_group_to_a_shared_edge() {
        let draft = vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 2000, 0, "1920x1080"),
        ];
        let start = [(1, 2000, 0)];
        let (dx, _, guide, _) = snap_group(&draft, &[1], &start, -70, 0, 20.0);
        assert_eq!(dx, -80);
        assert_eq!(guide, Some(1920));
    }

    #[test]
    fn panel_layout_has_no_control_collisions_at_the_minimum_size() {
        let mut state = state_with(
            (0..6)
                .map(|index| output(&format!("o{index}"), index * 1920, 0, "1920x1080"))
                .collect(),
        );
        state.draft[0].primary = true;
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        let mut rects = vec![
            ui.panel.primary,
            ui.panel.enabled,
            ui.panel.rotate,
            ui.panel.identify,
            ui.panel.mode,
            ui.panel.rate,
            ui.panel.single,
            ui.panel.extend,
            ui.panel.mirror,
            ui.panel.apply[0],
            ui.panel.apply[1],
            ui.panel.apply[2],
        ];
        for (left, rect) in rects.iter().enumerate() {
            for other in rects.iter().skip(left + 1) {
                assert!(
                    !rects_overlap(*rect, *other),
                    "controls overlap: {rect:?} {other:?}"
                );
            }
        }
        rects.extend(ui.panel.rows.iter().map(|(_, rect)| *rect));
        assert!(ui.panel.rows.len() >= 4);
    }

    #[test]
    fn stable_numbers_survive_a_disabled_screen() {
        let mut state = state_with(vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1920, 0, "1920x1080"),
        ]);
        state.draft[0].enabled = false;
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        let view = ui
            .monitors
            .iter()
            .find(|view| view.members.contains(&1))
            .unwrap();
        assert_eq!(view.members[0], 1);
        assert_eq!(view.index, 1);
    }

    #[test]
    fn adjacent_screens_do_not_get_a_false_overlap_from_rounding() {
        let state = state_with(vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1920, 0, "1920x1080"),
        ]);
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert_eq!(ui.monitors.len(), 2);
        assert!(ui.monitors.iter().all(|view| !view.overlapping));
        assert!(validate_draft(&state.draft)
            .iter()
            .all(|issue| !issue.blocking));
    }

    #[test]
    fn output_and_profile_pages_reach_every_item() {
        let draft = (0..8)
            .map(|index| output(&format!("o{index}"), index * 1920, 0, "1920x1080"))
            .collect();
        let mut state = state_with(draft);
        let mut profiles = Vec::new();
        for index in 0..11 {
            profiles.push(backend::Profile {
                name: format!("p{index}"),
                outputs: Vec::new(),
                ..Default::default()
            });
        }
        state.status.profiles = profiles;
        let first = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert!(first.panel.rows.len() < 8);
        assert!(first.panel.page_next.is_some());
        let per_page = first.profiles.per_page;
        assert!(per_page >= 4, "two columns of profile rows: {per_page}");
        assert_eq!(first.profiles.rows.len(), per_page);
        assert!(first.profiles.next.is_some());
        state.output_scroll = 2;
        page_profiles(&mut state, &first, 1);
        let second = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert_eq!(second.panel.rows.first().map(|(index, _)| *index), Some(2));
        assert_eq!(
            second.profiles.rows.first().map(|(index, _)| *index),
            Some(per_page)
        );
        // Every profile is on some page.
        let mut seen = Vec::new();
        state.profile_scroll = 0;
        for _ in 0..second.profiles.pages {
            let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
            seen.extend(ui.profiles.rows.iter().map(|(index, _)| *index));
            page_profiles(&mut state, &ui, 1);
        }
        assert_eq!(seen, (0..11).collect::<Vec<_>>());
        // Selecting a profile reveals its page.
        state.profile_scroll = 10;
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert!(ui.profiles.rows.iter().any(|(index, _)| *index == 10));
    }

    #[test]
    fn keyboard_selection_scrolls_the_screen_page() {
        let mut state = state_with(
            (0..8)
                .map(|index| output(&format!("o{index}"), index * 1920, 0, "1920x1080"))
                .collect(),
        );
        state.selected = 7;
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        handle_arrows(&mut state, &ui, K_DOWN, 1);
        assert!(state.selected >= state.output_scroll);
        assert!(state.selected < state.output_scroll + ui.panel.visible);
    }

    #[test]
    fn preview_comparison_ignores_output_order() {
        let mut state = state_with(vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1920, 0, "1920x1080"),
        ]);
        let mut reversed = state.draft.clone();
        reversed.reverse();
        state.preview_layout = Some(reversed);
        assert!(!state.preview_changed());
        state.draft[0].x = 10;
        assert!(state.preview_changed());
    }

    fn output_state(name: &str, modes: &[(&str, &[f64])]) -> backend::OutputState {
        backend::OutputState {
            name: name.into(),
            connected: true,
            active: true,
            modes: modes
                .iter()
                .map(|(mode, rates)| backend::DisplayMode {
                    mode: (*mode).into(),
                    rates: rates.to_vec(),
                    preferred: false,
                })
                .collect(),
            ..backend::OutputState::default()
        }
    }

    #[test]
    fn mirroring_uses_a_resolution_and_rate_supported_by_every_screen() {
        let mut draft = vec![
            output("a", 0, 0, "2560x1440"),
            output("b", 0, 0, "2560x1440"),
        ];
        draft[0].rate = Some(30.0);
        draft[0].primary = true;
        let mut state = state_with(draft);
        state.status.outputs = vec![
            output_state("a", &[("2560x1440", &[30.0, 60.0]), ("1920x1080", &[60.0])]),
            output_state("b", &[("2560x1440", &[60.0]), ("1280x720", &[50.0])]),
        ];
        state.mirror_screens();
        assert_eq!(state.draft[0].mode, "2560x1440");
        assert_eq!(state.draft[1].mode, "2560x1440");
        assert_eq!(state.draft[0].rate, Some(60.0));
        assert_eq!(state.draft[1].rate, Some(60.0));
    }

    #[test]
    fn mirroring_refuses_a_group_without_a_common_mode() {
        let draft = vec![
            output("a", 0, 0, "2560x1440"),
            output("b", 0, 0, "1920x1080"),
        ];
        let mut state = state_with(draft);
        state.status.outputs = vec![
            output_state("a", &[("2560x1440", &[60.0])]),
            output_state("b", &[("1920x1080", &[60.0])]),
        ];
        let before = state.draft.clone();
        state.mirror_screens();
        assert_eq!(state.draft, before);
        assert!(state.message.contains("no common resolution"));
    }

    #[test]
    fn disabled_outputs_do_not_move_the_active_layout() {
        let mut draft = vec![
            output("active", 0, 0, "1920x1080"),
            output("off", -800, -600, "1920x1080"),
        ];
        draft[1].enabled = false;
        assert!(!normalize_draft(&mut draft));
        assert_eq!((draft[0].x, draft[0].y), (0, 0));
        assert_eq!((draft[1].x, draft[1].y), (-800, -600));
    }

    #[test]
    fn an_unparseable_enabled_mode_blocks_preview() {
        let state = state_with(vec![output("a", 0, 0, "not-a-mode")]);
        assert!(validate_draft(&state.draft)
            .iter()
            .any(|issue| issue.blocking));
    }

    #[test]
    fn the_first_window_starts_centered_and_stays_on_screen() {
        assert_eq!(
            super::initial_position(1920, 1200),
            ((1920 - 1120) / 2, (1200 - 760) / 2)
        );
        assert_eq!(super::initial_position(800, 600), (0, 0));
    }

    #[test]
    fn normal_hints_advertise_only_a_sensible_minimum() {
        let hints = super::window_size_hints();
        assert_eq!(hints[0], 1 << 4, "only PMinSize is set");
        assert_eq!(hints[5], u32::from(super::SMALLEST_WIDTH));
        assert_eq!(hints[6], u32::from(super::SMALLEST_HEIGHT));
        assert_eq!(hints[7], 0, "no 1x1 maximum");
        assert_eq!(hints[8], 0, "no 1x1 maximum");
        assert_eq!(hints[9], 0, "no resize increment");
        assert_eq!(hints[10], 0, "no resize increment");
    }

    #[test]
    fn dragging_a_mirror_group_moves_both_members() {
        let mut state = state_with(vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 0, 0, "1920x1080"),
            output("c", 1920, 0, "1920x1080"),
        ]);
        state.draft[0].primary = true;
        state.move_selected(10, 5);
        assert_eq!((state.draft[0].x, state.draft[0].y), (10, 5));
        assert_eq!((state.draft[1].x, state.draft[1].y), (10, 5));
        assert_eq!((state.draft[2].x, state.draft[2].y), (1920, 0));
    }

    fn profile(name: &str, outputs: &[&str]) -> backend::Profile {
        backend::Profile {
            name: name.into(),
            outputs: outputs
                .iter()
                .enumerate()
                .map(|(index, output)| backend::ProfileOutput {
                    name: (*output).into(),
                    mode: "1920x1080".into(),
                    rate: Some(60.0),
                    position: backend::position_string(index as i32 * 1920, 0),
                    rotation: "normal".into(),
                    primary: index == 0,
                    label: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn done(after: After, status: backend::Status) -> backend::Done<After> {
        backend::Done {
            seq: 1,
            after,
            result: Ok(()),
            status: Ok(status),
        }
    }

    #[test]
    fn save_as_an_other_existing_name_asks_and_the_loaded_name_updates() {
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        state.status.profiles = vec![profile("Home", &["a"]), profile("Work", &["a"])];
        state.selected_profile = Some(0);
        assert!(
            !state.save_needs_confirm("Home"),
            "the loaded profile updates"
        );
        assert!(state.save_needs_confirm(" Work "), "another profile asks");
        assert!(!state.save_needs_confirm("New"), "a new name just saves");
        assert!(!state.save_needs_confirm("  "));
        state.selected_profile = None;
        assert!(state.save_needs_confirm("Home"), "nothing loaded: asks");
        // Replace only saves once confirmed.
        state.confirm = Some(Confirm::Replace("Work".into()));
        confirm_action(&mut state, 1);
        assert!(state.outbox.is_empty(), "Cancel saves nothing");
        state.confirm = Some(Confirm::Replace("Work".into()));
        confirm_action(&mut state, 0);
        assert!(matches!(
            state.outbox.first(),
            Some((backend::Job::Save { name, .. }, After::Save { update: true, .. })) if name == "Work"
        ));
        assert!(state.busy);
    }

    #[test]
    fn r_keeps_the_edited_draft_one_undo_away() {
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        state.draft[0].primary = true;
        state.move_selected(100, 0);
        let edited = state.draft.clone();
        assert!(state.dirty);
        let status = backend::Status {
            outputs: vec![output_state("a", &[("1920x1080", &[60.0])])],
            ..Default::default()
        };
        state.request_reload(true);
        let (job, after) = state.outbox.pop().unwrap();
        assert_eq!(job, backend::Job::Status);
        state.finish(done(after, status));
        assert!(!state.dirty);
        assert_eq!(state.draft[0].x, 0, "the running layout is shown");
        assert_eq!(state.message, "Reloaded. Ctrl+Z restores your draft.");
        state.undo();
        assert_eq!(state.draft, edited, "Ctrl+Z restores the draft");
        assert!(state.dirty);
        // Earlier history survives the reload too.
        state.undo();
        assert_eq!(state.draft[0].x, 0);
    }

    #[test]
    fn a_single_enabled_screen_is_primary_without_a_warning() {
        let status = backend::Status {
            outputs: vec![output_state("eDP-1", &[("1920x1080", &[60.0])])],
            ..Default::default()
        };
        assert!(!status.outputs[0].primary, "RandR reports no primary");
        let state = State::new(status);
        assert!(state.draft[0].primary);
        assert!(validate_draft(&state.draft).is_empty(), "no nag");
        assert!(
            state.profile_from_draft().outputs[0].primary,
            "sent as primary"
        );
        assert!(!state.dirty);
        // Two enabled screens without a primary still warn, once.
        let two = vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1920, 0, "1920x1080"),
        ];
        let issues = validate_draft(&two);
        assert_eq!(issues.len(), 1);
        assert!(!issues[0].blocking);
    }

    #[test]
    fn closing_during_a_preview_asks_and_keep_closes_after_niructl_answers() {
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        state.status.preview = Some(backend::Preview {
            name: "Draft".into(),
            token: "t1".into(),
            seconds_left: 12,
        });
        request_close(&mut state);
        assert_eq!(state.confirm, Some(Confirm::ClosePreview));
        assert!(!state.quit, "closing never reverts or quits silently");
        assert_eq!(
            Confirm::ClosePreview.buttons(),
            &["Keep layout", "Revert", "Cancel"]
        );
        confirm_action(&mut state, 2);
        assert!(state.confirm.is_none() && state.outbox.is_empty(), "Cancel");
        request_close(&mut state);
        confirm_action(&mut state, 0);
        let (job, after) = state.outbox.pop().unwrap();
        assert_eq!(job, backend::Job::Keep);
        assert!(!state.quit, "waits for the answer");
        state.finish(done(after, backend::Status::default()));
        assert!(state.quit);
        // Revert closes the same way.
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        state.status.preview = state_with(Vec::new())
            .status
            .preview
            .or(Some(backend::Preview {
                name: "Draft".into(),
                token: "t2".into(),
                seconds_left: 3,
            }));
        request_close(&mut state);
        confirm_action(&mut state, 1);
        assert_eq!(state.outbox[0].0, backend::Job::Revert);
    }

    #[test]
    fn a_late_status_never_replaces_a_newer_one() {
        let mut state = state_with(Vec::new());
        let newer = backend::Status {
            profiles: vec![profile("New", &[])],
            ..Default::default()
        };
        let mut answer = done(
            After::Reload {
                forced: false,
                background: true,
            },
            newer,
        );
        answer.seq = 5;
        state.finish(answer);
        let mut answer = done(
            After::Reload {
                forced: false,
                background: true,
            },
            backend::Status::default(),
        );
        answer.seq = 4;
        state.finish(answer);
        assert_eq!(state.status.profiles.len(), 1);
    }

    /// Every control rectangle of the panel and the profile strip.
    fn controls(ui: &Ui) -> Vec<Rectangle> {
        let mut rects = vec![
            ui.panel.primary,
            ui.panel.enabled,
            ui.panel.rotate,
            ui.panel.identify,
            ui.panel.mode,
            ui.panel.rate,
            ui.panel.single,
            ui.panel.extend,
            ui.panel.mirror,
            ui.panel.apply[0],
            ui.panel.apply[1],
            ui.panel.apply[2],
        ];
        rects.extend(ui.panel.rows.iter().map(|(_, rect)| *rect));
        rects.extend(ui.panel.page_prev);
        rects.extend(ui.panel.page_next);
        rects.extend(ui.profiles.rows.iter().map(|(_, rect)| *rect));
        rects.extend(ui.profiles.actions);
        rects.extend(ui.profiles.prev);
        rects.extend(ui.profiles.next);
        rects.extend(ui.profiles.automatic);
        rects
    }

    #[test]
    fn the_compact_layout_has_no_collisions_at_560x420_and_scrolls_to_everything() {
        let mut state = state_with(
            (0..6)
                .map(|index| output(&format!("o{index}"), index * 1920, 0, "1920x1080"))
                .collect(),
        );
        state.draft[0].primary = true;
        state.status.profiles = (0..11)
            .map(|i| profile(&format!("p{i}"), &["o0"]))
            .collect();
        // niructl 0.17 reports automatic switching: the header has a toggle.
        state.status.automatic = Some(backend::Automatic::default());
        for (w, h) in [(560, 420), (700, 560), (899, 699), (1400, 600), (560, 900)] {
            let l = layout(w, h);
            assert!(!l.too_small, "{w}x{h}");
            let viewport = l.viewport.expect("compact below 900x700");
            // The regions never overlap and stay inside the window width.
            for (left, right) in [
                (l.canvas, l.panel),
                (l.canvas, l.profiles),
                (l.panel, l.profiles),
            ] {
                assert!(!rects_overlap(left, right), "{w}x{h}: {left:?} {right:?}");
            }
            for region in [l.canvas, l.panel, l.profiles] {
                assert!(
                    region.x >= 0 && i32::from(region.x) + i32::from(region.width) <= i32::from(w)
                );
                assert!(region.width > 0 && region.height > 0, "{w}x{h}: {region:?}");
            }
            assert!(
                l.panel.height as i16 >= PANEL_MIN_H,
                "the panel is never squeezed"
            );
            // Scrolled to the end, the profile strip is fully in view.
            let end = layout_scrolled(w, h, i16::MAX);
            let bottom = i32::from(end.profiles.y) + i32::from(end.profiles.height);
            assert!(
                bottom <= i32::from(viewport.y) + i32::from(viewport.height),
                "{w}x{h}"
            );
            assert!(
                i32::from(end.profiles.y) >= i32::from(viewport.y),
                "{w}x{h}"
            );
            for scroll in [0, end.max_scroll / 2, end.max_scroll] {
                let ui = Ui::scrolled(&state, w, h, None, 1.0, (0.0, 0.0), scroll);
                let rects = controls(&ui);
                for (left, rect) in rects.iter().enumerate() {
                    for other in rects.iter().skip(left + 1) {
                        assert!(
                            !rects_overlap(*rect, *other),
                            "{w}x{h}+{scroll}: controls overlap: {rect:?} {other:?}"
                        );
                    }
                }
                assert!(!ui.panel.rows.is_empty());
            }
        }
        // The full layout is unchanged at its minimum.
        let full = layout(MIN_WIDTH, MIN_HEIGHT);
        assert!(full.viewport.is_none() && full.max_scroll == 0);
        // Below the smallest size the window asks to be made larger.
        assert!(layout(559, 420).too_small);
        assert!(layout(560, 419).too_small);
        let ui = Ui::new(&state, 400, 300, None, 1.0, (0.0, 0.0));
        assert_eq!(hit(&state, &ui, 10, 10), None);
    }

    #[test]
    fn compact_controls_outside_the_viewport_cannot_be_hit() {
        let state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        let ui = Ui::scrolled(&state, 560, 420, None, 1.0, (0.0, 0.0), i16::MAX);
        let viewport = ui.l.viewport.unwrap();
        // The canvas is scrolled up under the header: its pixels there are
        // the header's, not a screen's.
        let x = ui.l.canvas.x + 10;
        let y = viewport.y - 5;
        assert_eq!(hit(&state, &ui, x, y), None);
    }

    fn connected(name: &str) -> backend::OutputState {
        output_state(name, &[("1920x1080", &[60.0]), ("2560x1440", &[59.95])])
    }

    #[test]
    fn profiles_show_availability_current_login_and_a_summary() {
        let mut status = backend::Status {
            outputs: vec![connected("eDP-1"), connected("DisplayPort-1")],
            startup_profile: Some("Desk".into()),
            ..Default::default()
        };
        status.outputs[0].current = Some(backend::CurrentOutput {
            mode: "1920x1080".into(),
            rate: Some(60.0),
            position: "0x0".into(),
            rotation: "normal".into(),
            primary: true,
        });
        status.outputs[0].primary = true;
        status.outputs[1].active = false;
        let desk = backend::Profile {
            name: "Desk".into(),
            outputs: ["eDP-1", "DisplayPort-7", "DisplayPort-8"]
                .iter()
                .enumerate()
                .map(|(index, name)| backend::ProfileOutput {
                    name: (*name).into(),
                    mode: if index == 0 { "1920x1080" } else { "2560x1440" }.into(),
                    rate: Some(60.0),
                    position: backend::position_string(index as i32 * 2560, 0),
                    rotation: "normal".into(),
                    primary: index == 0,
                    label: None,
                })
                .collect(),
            ..Default::default()
        };
        let info = profile_info(&status, &desk);
        assert_eq!(info.missing, vec!["DisplayPort-7", "DisplayPort-8"]);
        assert_eq!(info.summary, "3 screens · eDP + 2\u{d7} 2560x1440");
        assert!(info.startup && !info.current);
        // The running layout: only eDP-1 at 1920x1080.
        let laptop = profile("Laptop", &["eDP-1"]);
        let info = profile_info(&status, &laptop);
        assert!(info.missing.is_empty() && info.current && !info.startup);
        assert_eq!(info.summary, "1 screen · eDP");
        // A different mode, position or an extra screen is not current.
        let mut moved = laptop.clone();
        moved.outputs[0].position = "10x0".into();
        assert!(!profile_info(&status, &moved).current);
        let both = profile("Both", &["eDP-1", "DisplayPort-1"]);
        let info = profile_info(&status, &both);
        assert!(info.missing.is_empty() && !info.current);
        assert_eq!(info.summary, "2 screens · eDP + 1920x1080");
        // A profile without a rate matches any rate.
        let mut any_rate = laptop;
        any_rate.outputs[0].rate = None;
        assert!(profile_info(&status, &any_rate).current);
    }

    #[test]
    fn an_unusable_profile_is_selected_but_not_loaded() {
        let mut state = state_with(Vec::new());
        state.status = backend::Status {
            outputs: vec![connected("eDP-1")],
            profiles: vec![profile("TV", &["HDMI-1"]), profile("Laptop", &["eDP-1"])],
            ..Default::default()
        };
        state.draft = draft_from_status(&state.status);
        let before = state.draft.clone();
        assert!(!state.select_profile(0));
        assert_eq!(state.selected_profile, Some(0), "Rename/Delete still work");
        assert_eq!(state.draft, before, "the draft is untouched");
        assert!(state.message.contains("needs HDMI-1"));
        assert!(state.select_profile(1));
        assert_eq!(state.selected_profile, Some(1));
    }

    fn popover_state() -> State {
        let mut state = state_with(vec![
            output("a", 0, 0, "1920x1080"),
            output("b", 1920, 0, "1920x1080"),
        ]);
        state.draft[0].primary = true;
        let mut a = output_state(
            "a",
            &[
                ("1920x1080", &[60.0, 50.0]),
                ("3840x2160", &[30.0]),
                ("1280x720", &[60.0]),
                ("2560x1440", &[59.95, 143.91]),
            ],
        );
        a.modes[1].preferred = true;
        state.status.outputs = vec![a, output_state("b", &[("1920x1080", &[60.0])])];
        state
    }

    fn labels(state: &State) -> Vec<String> {
        state
            .popover
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|item| item.label.clone())
            .collect()
    }

    #[test]
    fn the_mode_list_is_sorted_by_size_and_keyboard_driven() {
        let mut state = popover_state();
        state.open_popover(PopKind::Mode).unwrap();
        assert_eq!(state.focus, Some(Hit::Mode));
        assert_eq!(
            labels(&state),
            vec!["3840x2160", "2560x1440", "1920x1080", "1280x720"]
        );
        let popover = state.popover.as_ref().unwrap();
        assert_eq!(popover.items[0].note, Some("preferred"));
        assert!(popover.items[2].checked, "the current mode is checked");
        assert_eq!(popover.cursor, 2, "the cursor starts on the current mode");
        // Up moves, Enter chooses and closes; the rate follows the mode.
        assert!(state.popover_key(K_UP));
        assert!(state.popover_key(K_RETURN));
        assert!(state.popover.is_none());
        assert_eq!(state.draft[0].mode, "2560x1440");
        assert_eq!(state.draft[0].rate, Some(59.95));
        assert!(state.dirty);
        // Esc closes without a change.
        state.open_popover(PopKind::Mode).unwrap();
        state.popover_key(K_HOME);
        assert!(state.popover_key(K_ESCAPE));
        assert!(state.popover.is_none());
        assert_eq!(state.draft[0].mode, "2560x1440");
        // Rates for the chosen mode, highest first.
        state.open_popover(PopKind::Rate).unwrap();
        assert_eq!(labels(&state), vec!["143.91 Hz", "59.95 Hz"]);
        state.popover_key(K_UP);
        state.popover_key(K_SPACE);
        assert_eq!(state.draft[0].rate, Some(143.91));
        // Rotation lists four entries.
        state.open_popover(PopKind::Rotation).unwrap();
        assert_eq!(labels(&state), vec!["normal", "left", "inverted", "right"]);
        state.popover_key(K_END);
        state.popover_key(K_RETURN);
        assert_eq!(state.draft[0].rotation, "right");
        // Ctrl+Z undoes a list choice like any other edit.
        state.undo();
        assert_eq!(state.draft[0].rotation, "normal");
        // Left/right on the focused buttons still cycle.
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        state.focus = Some(Hit::Rotate);
        handle_arrows(&mut state, &ui, K_RIGHT, 1);
        assert_eq!(state.draft[0].rotation, "left");
    }

    #[test]
    fn a_mirror_group_lists_only_common_modes_and_the_list_fits_the_window() {
        let mut state = popover_state();
        state.draft[1].x = 0;
        state.open_popover(PopKind::Mode).unwrap();
        assert_eq!(labels(&state), vec!["1920x1080"]);
        state.popover = None;
        state.open_popover(PopKind::Rate).unwrap();
        assert_eq!(labels(&state), vec!["60.00 Hz"]);
        // A long list scrolls with the cursor and stays inside the window.
        let modes: Vec<(String, Vec<f64>)> = (0..30)
            .map(|i| (format!("{}x{}", 800 + i * 40, 600 + i * 20), vec![60.0]))
            .collect();
        let refs: Vec<(&str, &[f64])> = modes
            .iter()
            .map(|(m, r)| (m.as_str(), r.as_slice()))
            .collect();
        let mut state = state_with(vec![output("a", 0, 0, "800x600")]);
        state.status.outputs = vec![output_state("a", &refs)];
        state.open_popover(PopKind::Mode).unwrap();
        state.popover_key(K_END);
        for (w, h) in [(MIN_WIDTH, MIN_HEIGHT), (560, 420)] {
            let ui = Ui::new(&state, w, h, None, 1.0, (0.0, 0.0));
            let layout = popover_layout(&state, &ui, w, h).unwrap();
            assert!(layout.rows.iter().any(|(index, _)| *index == 29), "{w}x{h}");
            assert!(layout.rows.len() <= POP_ROWS);
            assert!(layout.rect.y >= 0);
            assert!(i32::from(layout.rect.y) + i32::from(layout.rect.height) <= i32::from(h));
            let (_, row) = layout.rows.last().unwrap();
            assert_eq!(
                hit(&state, &ui, row.x + 5, row.y + 5),
                Some(Hit::PopItem(29))
            );
            assert_eq!(hit(&state, &ui, 2, 2), None, "outside closes, never acts");
        }
        state.popover_key(K_HOME);
        assert_eq!(state.popover.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn disabled_controls_explain_themselves() {
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        assert_eq!(
            disabled_reason(&state, Hit::Keep),
            Some("Keep is available during a preview")
        );
        assert_eq!(
            disabled_reason(&state, Hit::Revert),
            Some("Revert is available during a preview")
        );
        assert_eq!(
            disabled_reason(&state, Hit::Extend),
            Some("Extend needs two connected screens")
        );
        assert_eq!(
            disabled_reason(&state, Hit::Mirror),
            Some("Mirror needs two enabled screens")
        );
        assert_eq!(
            disabled_reason(&state, Hit::Delete),
            Some("Select a profile first")
        );
        assert_eq!(disabled_reason(&state, Hit::Apply), None);
        assert_eq!(disabled_reason(&state, Hit::Mode), None);
        state.draft[0].enabled = false;
        assert!(disabled_reason(&state, Hit::Mode).is_some());
        assert!(disabled_reason(&state, Hit::Primary).is_some());
        state.draft[0].enabled = true;
        state.status.preview = Some(backend::Preview {
            name: "Draft".into(),
            token: "t".into(),
            seconds_left: 9,
        });
        assert_eq!(disabled_reason(&state, Hit::Keep), None);
        state.busy = true;
        assert!(disabled_reason(&state, Hit::Keep)
            .unwrap()
            .starts_with("Working"));
        assert_eq!(
            disabled_reason(&state, Hit::Monitor(0)),
            None,
            "screens stay"
        );
    }

    #[test]
    fn the_preview_countdown_runs_from_a_local_deadline() {
        let mut state = state_with(vec![output("a", 0, 0, "1920x1080")]);
        let status = backend::Status {
            preview: Some(backend::Preview {
                name: "Draft".into(),
                token: "t1".into(),
                seconds_left: 15,
            }),
            ..Default::default()
        };
        state.finish(backend::Done {
            seq: 1,
            after: After::Preview {
                layout: state.draft.clone(),
            },
            result: Ok(()),
            status: Ok(status),
        });
        assert!(state.own_preview, "started here: Enter keeps, Esc reverts");
        let now = Instant::now();
        assert_eq!(state.preview_seconds_left(now), Some(15));
        assert_eq!(
            state.preview_seconds_left(now + Duration::from_millis(4500)),
            Some(11)
        );
        assert_eq!(
            state.preview_seconds_left(now + Duration::from_secs(20)),
            Some(0)
        );
        assert!(state.preview_fraction(now) > 0.95);
        assert!(state.preview_fraction(now + Duration::from_millis(7500)) < 0.55);
        // A preview adopted from elsewhere is not this window's.
        let other = backend::Status {
            preview: Some(backend::Preview {
                name: "CLI".into(),
                token: "t2".into(),
                seconds_left: 8,
            }),
            ..Default::default()
        };
        state.apply_status(other, false);
        assert!(!state.own_preview);
        assert_eq!(state.preview_seconds_left(Instant::now()), Some(8));
        state.apply_status(backend::Status::default(), false);
        assert_eq!(state.preview_seconds_left(Instant::now()), None);
        assert!(state.message.starts_with("Preview ended"));
    }

    #[test]
    fn the_next_countdown_tick_is_on_the_second() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(4300);
        assert_eq!(next_second(deadline, now), now + Duration::from_millis(300));
        assert_eq!(
            next_second(now + Duration::from_secs(3), now),
            now + Duration::from_secs(1)
        );
    }

    fn automatic_status(enabled: bool, last_profile: Option<&str>) -> backend::Automatic {
        backend::Automatic {
            enabled,
            last: last_profile.map(|name| backend::AutoEvent {
                time: 1,
                action: "profile".into(),
                profile: Some(name.into()),
                detail: String::new(),
                ok: true,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn rows_show_auto_manual_and_niructls_reason() {
        let mut status = backend::Status {
            outputs: vec![connected("eDP-1"), connected("DisplayPort-1")],
            ..Default::default()
        };
        status.outputs[0].current = Some(backend::CurrentOutput {
            mode: "1920x1080".into(),
            rate: Some(60.0),
            position: "0x0".into(),
            rotation: "normal".into(),
            primary: true,
        });
        status.outputs[1].active = false;
        status.automatic = Some(automatic_status(true, Some("Laptop")));
        let laptop = profile("Laptop", &["eDP-1"]);
        let info = profile_info(&status, &laptop);
        assert!(info.current && info.auto_applied && !info.manual_only);
        // The same profile applied by hand is current but not "auto".
        status.automatic = Some(automatic_status(true, Some("Other")));
        assert!(!profile_info(&status, &laptop).auto_applied);
        // Automation off: never "auto".
        status.automatic = Some(automatic_status(false, Some("Laptop")));
        assert!(!profile_info(&status, &laptop).auto_applied);
        // niructl's verdict wins over connector names: the connector exists,
        // but the screen with the saved identity is not there.
        let mut desk = profile("Desk", &["DisplayPort-1"]);
        desk.fit = Some("missing".into());
        desk.loadable = Some(false);
        desk.reason = Some("needs HPN HP E27q G5 (serial …2MRJ)".into());
        desk.auto = false;
        let info = profile_info(&status, &desk);
        assert_eq!(
            info.missing,
            vec!["needs HPN HP E27q G5 (serial …2MRJ)".to_string()]
        );
        assert!(info.manual_only);
        assert_eq!(
            missing_outputs(&status, &desk),
            vec!["HPN HP E27q G5 (serial …2MRJ)".to_string()]
        );
        // Hidden by the closed lid: not usable now, but it can be loaded.
        let mut lid = profile("Lid", &["eDP-1"]);
        lid.fit = Some("missing".into());
        lid.loadable = Some(true);
        lid.reason = Some("needs the built-in screen (the lid is closed)".into());
        let info = profile_info(&status, &lid);
        assert_eq!(info.missing.len(), 1);
        assert!(missing_outputs(&status, &lid).is_empty());
    }

    #[test]
    fn an_automatic_layout_is_offered_for_saving_until_it_is_a_profile() {
        let mut state = state_with(vec![output("eDP-1", 0, 0, "1920x1080")]);
        assert_eq!(
            state.automatic_layout(),
            None,
            "an older niructl offers nothing"
        );
        assert!(state.toggle_automatic().is_err());
        state.status.automatic = Some(backend::Automatic {
            enabled: true,
            automatic_layout: Some("built-in screen + HP E27q G5".into()),
            ..Default::default()
        });
        assert_eq!(
            state.automatic_layout(),
            Some("built-in screen + HP E27q G5")
        );
        state.dirty = true;
        assert_eq!(
            state.automatic_layout(),
            None,
            "an edited draft is the user's own"
        );
        state.dirty = false;
        state.status.automatic.as_mut().unwrap().current_profile = Some("Desk".into());
        assert_eq!(
            state.automatic_layout(),
            None,
            "a profile is shown, nothing to save"
        );
        // The toggle asks niructl to turn the automation off.
        state.toggle_automatic().unwrap();
        let (job, after) = state.outbox.pop().unwrap();
        assert_eq!(job, backend::Job::Automatic { enabled: false });
        assert!(matches!(after, After::Automatic { enabled: false }));
        assert!(state.busy);
    }

    #[test]
    fn the_automatic_toggle_sits_in_the_header_clear_of_the_title_and_pager() {
        let mut state = state_with(vec![output("eDP-1", 0, 0, "1920x1080")]);
        state.status.automatic = Some(backend::Automatic::default());
        for profiles in [1usize, 30] {
            state.status.profiles = (0..profiles)
                .map(|i| profile(&format!("p{i}"), &["eDP-1"]))
                .collect();
            for (w, h) in [(560u16, 420u16), (900, 700), (1400, 900)] {
                let ui = Ui::new(&state, w, h, None, 1.0, (0.0, 0.0));
                let toggle = ui.profiles.automatic.expect("toggle");
                assert!(
                    toggle.x >= ui.l.profiles.x + 78,
                    "{w}x{h}: clear of the title"
                );
                assert!(contains(ui.l.profiles, toggle.x, toggle.y));
                for rect in [ui.profiles.prev, ui.profiles.next].into_iter().flatten() {
                    assert!(!rects_overlap(rect, toggle), "{w}x{h}: {rect:?} {toggle:?}");
                }
                for (_, row) in &ui.profiles.rows {
                    assert!(!rects_overlap(*row, toggle));
                }
                if ui.l.viewport.is_none() {
                    assert_eq!(
                        hit(&state, &ui, toggle.x + 2, toggle.y + 2),
                        Some(Hit::Automatic),
                        "{w}x{h}"
                    );
                }
                assert!(focus_order(&ui).contains(&Hit::Automatic));
            }
        }
    }
}
