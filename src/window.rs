//! The NIRU Display window.
//!
//! The window is a front end for `niructl display`. It edits a local draft and
//! sends that draft through the same preview/keep/revert transaction the CLI
//! and the launcher use. It owns no profile store and runs no display daemon.

use crate::{backend, text, theme};
use std::time::{Duration, Instant};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT,
};

pub const APP_ID: &str = "nirudisplay";
pub const APP_NAME: &str = "NIRU Display";

const WIDTH: u16 = 1120;
const HEIGHT: u16 = 760;
const MIN_WIDTH: u16 = 900;
const MIN_HEIGHT: u16 = 700;

const PAD: i16 = 20;
const HEADER: i16 = 76;
const FOOTER: i16 = 58;
const PANEL_W: i16 = 330;
const PROFILE_H: i16 = 138;
const ROW_H: i16 = 30;
const PROFILE_PAGE: usize = 8;

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
const K_SPACE: u32 = 0x0020;
const K_F1: u32 = 0xffbe;
const K_Q: u32 = b'q' as u32;
const K_Q_SHIFTED: u32 = b'Q' as u32;
const K_I: u32 = b'i' as u32;
const K_R: u32 = b'r' as u32;
const K_Z: u32 = b'z' as u32;
const K_0: u32 = b'0' as u32;
const K_N: u32 = b'n' as u32;
const K_N_SHIFTED: u32 = b'N' as u32;
const K_Y: u32 = b'y' as u32;
const K_Y_SHIFTED: u32 = b'Y' as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Monitor(usize),
    Output(usize),
    Enabled,
    Primary,
    Rotate,
    ModePrev,
    ModeNext,
    RatePrev,
    RateNext,
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
}

pub struct State {
    pub status: backend::Status,
    pub draft: Vec<DraftOutput>,
    pub selected: usize,
    pub selected_profile: Option<usize>,
    pub focus: Option<Hit>,
    pub message: String,
    pub message_is_error: bool,
    pub busy: bool,
    pub dirty: bool,
    pub name_mode: Option<NameAction>,
    pub name_text: String,
    pub output_scroll: usize,
    pub profile_scroll: usize,
    pub confirm: Option<Confirm>,
    pub quit: bool,
    had_preview: bool,
    preview_layout: Option<Vec<DraftOutput>>,
    undo: Vec<Vec<DraftOutput>>,
}

impl State {
    pub fn load() -> Result<Self, String> {
        let status = backend::read_status()?;
        backend::adopt_preview(status.preview.as_ref());
        let draft = draft_from_status(&status);
        let had_preview = status.preview.is_some();
        let message = status
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
        Ok(Self {
            status,
            draft: draft.clone(),
            selected: 0,
            selected_profile: None,
            focus: None,
            message,
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            quit: false,
            had_preview,
            preview_layout: had_preview.then_some(draft),
            undo: Vec::new(),
        })
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

    fn refresh_dirty_from_status(&mut self) {
        let actual = draft_from_status(&self.status);
        self.dirty = !layouts_match(&self.draft, &actual);
    }

    pub fn reload(&mut self, force_draft: bool) -> Result<(), String> {
        let status = backend::read_status()?;
        backend::adopt_preview(status.preview.as_ref());
        let was_preview = self.had_preview;
        let active = status.preview.is_some();
        if force_draft {
            self.undo.clear();
        }
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
        self.clamp_scrolls();
        Ok(())
    }

    /// Refresh profile/startup metadata without replacing an edited draft.
    pub fn refresh_profiles(&mut self) -> Result<(), String> {
        let status = backend::read_status()?;
        backend::adopt_preview(status.preview.as_ref());
        self.status = status;
        self.clamp_scrolls();
        if self.selected >= self.draft.len() {
            self.selected = self.draft.len().saturating_sub(1);
        }
        Ok(())
    }

    fn clamp_scrolls(&mut self) {
        let profile_pages = self.status.profiles.len().div_ceil(PROFILE_PAGE);
        self.profile_scroll = self.profile_scroll.min(profile_pages.saturating_sub(1));
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

    pub fn rotate(&mut self) {
        let group = self.mirror_group(self.selected);
        let Some(source) = self.current().cloned() else {
            return;
        };
        if !source.enabled {
            self.set_message("Enable the screen before rotating it.", true);
            return;
        }
        let rotation = match source.rotation.as_str() {
            "normal" => "left",
            "left" => "inverted",
            "inverted" => "right",
            _ => "normal",
        }
        .to_string();
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
        let normalized = normalized_draft(&self.draft);
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
                })
                .collect(),
        }
    }

    pub fn load_profile(&mut self, index: usize) -> Result<(), String> {
        let profile = self
            .status
            .profiles
            .get(index)
            .cloned()
            .ok_or("unknown display profile")?;
        let missing: Vec<String> = profile
            .outputs
            .iter()
            .filter(|output| {
                self.status
                    .output(&output.name)
                    .map(|state| !state.usable())
                    .unwrap_or(true)
            })
            .map(|output| output.name.clone())
            .collect();
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
        self.draft = draft;
        self.selected = self
            .draft
            .iter()
            .position(|output| output.enabled)
            .unwrap_or(0);
        self.selected_profile = Some(index);
        self.output_scroll = 0;
        self.profile_scroll = index / PROFILE_PAGE;
        self.dirty = true;
        self.set_message(
            format!("Loaded {}. Apply to preview it.", profile.name),
            false,
        );
        Ok(())
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
        backend::preview(&profile, 15)?;
        self.dirty = false;
        self.had_preview = true;
        self.preview_layout = Some(self.draft.clone());
        self.set_message(
            "Previewing for 15 s. Keep the result or let it revert.",
            false,
        );
        let _ = self.reload(false);
        Ok(())
    }

    pub fn keep(&mut self) -> Result<(), String> {
        if self.preview_changed() {
            return Err(
                "The draft changed after the preview; press Preview again to apply it.".into(),
            );
        }
        backend::keep()?;
        self.dirty = false;
        self.had_preview = false;
        self.preview_layout = None;
        self.set_message("Display layout kept.", false);
        let _ = self.reload(true);
        Ok(())
    }

    pub fn revert(&mut self) -> Result<(), String> {
        backend::revert()?;
        self.dirty = false;
        self.had_preview = false;
        self.preview_layout = None;
        self.set_message("Previous layout restored.", false);
        let _ = self.reload(true);
        Ok(())
    }

    pub fn save(&mut self, name: &str) -> Result<(), String> {
        if name.trim().is_empty() {
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
        profile.name = name.trim().to_string();
        backend::save_profile(&profile, name.trim())?;
        self.set_message(format!("Saved display profile {}.", name.trim()), false);
        self.refresh_profiles()?;
        self.refresh_dirty_from_status();
        self.selected_profile = self
            .status
            .profiles
            .iter()
            .position(|profile| profile.name == name.trim());
        if let Some(index) = self.selected_profile {
            self.profile_scroll = index / PROFILE_PAGE;
        }
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
        backend::rename_profile(&old, new.trim())?;
        self.refresh_profiles()?;
        self.selected_profile = self
            .status
            .profiles
            .iter()
            .position(|profile| profile.name == new.trim());
        if let Some(index) = self.selected_profile {
            self.profile_scroll = index / PROFILE_PAGE;
        }
        self.set_message(format!("Renamed {old} to {}.", new.trim()), false);
        Ok(())
    }

    pub fn delete_named(&mut self, name: &str) -> Result<(), String> {
        backend::delete_profile(name)?;
        self.refresh_profiles()?;
        self.refresh_dirty_from_status();
        self.selected_profile = None;
        self.set_message(format!("Deleted display profile {name}."), false);
        Ok(())
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
    draft
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
    if !enabled.iter().any(|output| output.primary) {
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
}

pub fn layout(w: u16, h: u16) -> Layout {
    let canvas_height = i16::try_from(h)
        .unwrap_or(i16::MAX)
        .saturating_sub(HEADER + FOOTER + PROFILE_H + 16);
    let canvas = Rectangle {
        x: PAD,
        y: HEADER,
        width: w.saturating_sub(u16::try_from(PAD * 2 + PANEL_W + 12).unwrap_or(0)),
        height: u16::try_from(canvas_height).unwrap_or(0),
    };
    let panel = Rectangle {
        x: i16::try_from(w).unwrap_or(i16::MAX) - PAD - PANEL_W,
        y: HEADER,
        width: PANEL_W as u16,
        height: canvas.height,
    };
    let profiles = Rectangle {
        x: PAD,
        y: canvas.y + i16::try_from(canvas.height).unwrap_or(0) + 10,
        width: w.saturating_sub(PAD as u16 * 2),
        height: PROFILE_H as u16,
    };
    let footer = Rectangle {
        x: PAD,
        y: i16::try_from(h).unwrap_or(i16::MAX) - FOOTER,
        width: w.saturating_sub(PAD as u16 * 2),
        height: FOOTER as u16,
    };
    Layout {
        canvas,
        panel,
        profiles,
        footer,
    }
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
    mode_prev: Rectangle,
    mode_next: Rectangle,
    rate_prev: Rectangle,
    rate_next: Rectangle,
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
    let quarter = (width.saturating_sub(gap * 3)) / 4;
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
    let fourth = |slot: i16| Rectangle {
        x: x + slot * (i16::try_from(quarter).unwrap_or(0) + gap_i),
        y: props_y + 60,
        width: quarter,
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
        mode_prev: fourth(0),
        mode_next: fourth(1),
        rate_prev: fourth(2),
        rate_next: fourth(3),
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
    actions: [Rectangle; 5],
    page: usize,
    pages: usize,
}

fn profile_layout(state: &State, l: Layout) -> ProfileLayout {
    let total = state.status.profiles.len();
    let pages = total.div_ceil(PROFILE_PAGE).max(1);
    let page = state.profile_scroll.min(pages.saturating_sub(1));
    let start = page * PROFILE_PAGE;
    let visible = (total.saturating_sub(start)).min(PROFILE_PAGE);
    let columns = 4usize;
    let width = (l.profiles.width.saturating_sub(18)) / columns as u16;
    let rows = (0..visible)
        .map(|slot| {
            let column = slot % columns;
            let row = slot / columns;
            (
                start + slot,
                Rectangle {
                    x: l.profiles.x + column as i16 * i16::try_from(width + 4).unwrap_or(0),
                    y: l.profiles.y + 30 + row as i16 * 31,
                    width,
                    height: 27,
                },
            )
        })
        .collect();
    let (prev, next) = if total > PROFILE_PAGE {
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
    let action_width = (l.profiles.width.saturating_sub(24)) / 5;
    let actions = std::array::from_fn(|index| Rectangle {
        x: l.profiles.x + index as i16 * i16::try_from(action_width + 4).unwrap_or(0),
        y: l.profiles.y + i16::try_from(l.profiles.height).unwrap_or(0) - 33,
        width: action_width,
        height: 27,
    });
    ProfileLayout {
        rows,
        prev,
        next,
        actions,
        page,
        pages,
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
    fn new(
        state: &State,
        width: u16,
        height: u16,
        frozen: Option<Canvas>,
        zoom: f32,
        pan: (f32, f32),
    ) -> Self {
        let l = layout(width, height);
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

fn confirm_buttons(w: u16, h: u16) -> (Rectangle, Rectangle) {
    let rect = confirm_rect(w, h);
    let width = (rect.width.saturating_sub(38)) / 2;
    (
        Rectangle {
            x: rect.x + 16,
            y: rect.y + i16::try_from(rect.height).unwrap_or(0) - 42,
            width,
            height: 28,
        },
        Rectangle {
            x: rect.x + 16 + i16::try_from(width).unwrap_or(0) + 6,
            y: rect.y + i16::try_from(rect.height).unwrap_or(0) - 42,
            width,
            height: 28,
        },
    )
}

fn hit(state: &State, ui: &Ui, x: i16, y: i16) -> Option<Hit> {
    if state.name_mode.is_some() {
        return Some(Hit::NameField);
    }
    if state.confirm.is_some() {
        return None;
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
        (ui.panel.mode_prev, Hit::ModePrev),
        (ui.panel.mode_next, Hit::ModeNext),
        (ui.panel.rate_prev, Hit::RatePrev),
        (ui.panel.rate_next, Hit::RateNext),
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
        Hit::ModePrev => Some(ui.panel.mode_prev),
        Hit::ModeNext => Some(ui.panel.mode_next),
        Hit::RatePrev => Some(ui.panel.rate_prev),
        Hit::RateNext => Some(ui.panel.rate_next),
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
        Hit::ModePrev,
        Hit::ModeNext,
        Hit::RatePrev,
        Hit::RateNext,
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
    out.extend([
        Hit::Save,
        Hit::Rename,
        Hit::Delete,
        Hit::Startup,
        Hit::Reload,
    ]);
    out
}

/// Keycode-to-keysym table for the connected keyboard.
pub struct Keys {
    table: Vec<u32>,
    min: u8,
}

impl Keys {
    pub fn new(c: &RustConnection) -> Result<Self, String> {
        let setup = c.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let map = c
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        let per = map.keysyms_per_keycode.max(1) as usize;
        let table = map
            .keysyms
            .chunks(per)
            .map(|syms| syms.first().copied().unwrap_or(0))
            .collect();
        Ok(Self {
            table,
            min: setup.min_keycode,
        })
    }

    pub fn sym(&self, code: u8) -> u32 {
        code.checked_sub(self.min)
            .and_then(|index| self.table.get(index as usize))
            .copied()
            .unwrap_or(0)
    }
}

fn text_key(sym: u32, shift: bool) -> Option<char> {
    if (0x20..=0x7e).contains(&sym) {
        let ch = char::from_u32(sym)?;
        return Some(if shift { ch.to_ascii_uppercase() } else { ch });
    }
    match sym {
        0x00e5 => Some('å'),
        0x00c5 => Some('Å'),
        0x00e4 => Some('ä'),
        0x00c4 => Some('Ä'),
        0x00f6 => Some('ö'),
        0x00d6 => Some('Ö'),
        _ => None,
    }
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
    keys: Keys,
    drag: Option<Drag>,
    pan_drag: Option<(i16, i16, f32, f32)>,
    zoom: f32,
    pan: (f32, f32),
    hover: Option<Hit>,
    cursors: Option<Cursors>,
    cursor: Option<Cursor>,
    overlays: Vec<Window>,
    identify_until: Option<Instant>,
}

impl App {
    pub fn open() -> Result<Self, String> {
        let (c, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = c.setup().roots[screen].root;
        let depth = c
            .get_geometry(root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .depth;
        if ![24, 32].contains(&depth) {
            return Err(format!(
                "NIRU Display needs a 24 or 32-bit screen, this one is {depth}"
            ));
        }
        let image_lsb =
            c.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST;
        let palette = theme::load();
        let font = text::Font::load(palette);
        let keys = Keys::new(&c)?;
        let win = c.generate_id().map_err(|e| e.to_string())?;
        let gc = c.generate_id().map_err(|e| e.to_string())?;
        let back = c.generate_id().map_err(|e| e.to_string())?;
        c.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
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
        let mut hints = vec![0u32; 18];
        hints[0] = 1 << 4 | 1 << 5 | 1 << 10 | 1 << 11;
        hints[5] = u32::from(MIN_WIDTH);
        hints[6] = u32::from(MIN_HEIGHT);
        hints[7] = 1;
        hints[8] = 1;
        hints[9] = PAD as u32;
        hints[10] = PAD as u32;
        c.change_property32(
            PropMode::REPLACE,
            win,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            &hints,
        )
        .map_err(|e| e.to_string())?;
        c.change_property32(
            PropMode::REPLACE,
            win,
            atom("WM_PROTOCOLS")?,
            AtomEnum::ATOM,
            &[atom("WM_DELETE_WINDOW")?],
        )
        .map_err(|e| e.to_string())?;
        let cursors = load_cursors(&c);
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
            drag: None,
            pan_drag: None,
            zoom: 1.0,
            pan: (0.0, 0.0),
            hover: None,
            cursors,
            cursor: None,
            overlays: Vec::new(),
            identify_until: None,
        })
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
        let (fill, fg) = if !enabled {
            (self.palette.background, self.palette.border)
        } else if active {
            (self.palette.accent, self.palette.background)
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
        self.c
            .set_clip_rectangles(ClipOrdering::UNSORTED, self.gc, 0, 0, &[rect])
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
    }

    fn unclip(&self) -> Result<(), String> {
        self.c
            .set_clip_rectangles(
                ClipOrdering::UNSORTED,
                self.gc,
                0,
                0,
                &[Rectangle {
                    x: 0,
                    y: 0,
                    width: self.w,
                    height: self.h,
                }],
            )
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())
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
        self.label(PAD, PAD - 8, APP_NAME, self.palette.foreground, 20.0)?;
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
        self.label(PAD, PAD + 22, &subtitle, self.palette.muted, 12.0)?;
        if state.dirty {
            self.label_right(
                i16::try_from(self.w).unwrap_or(i16::MAX) - PAD,
                PAD + 22,
                "Unsaved changes",
                self.palette.warning(),
                12.0,
            )?;
        }
        if let Some(preview) = &state.status.preview {
            self.label_right(
                i16::try_from(self.w).unwrap_or(i16::MAX) - PAD,
                PAD - 4,
                &format!("Preview {} s", preview.seconds_left),
                self.palette.accent,
                13.0,
            )?;
        }
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
        let bg = if !view.enabled {
            self.palette.background
        } else if view.selected {
            self.palette.screen_selected()
        } else {
            self.palette.screen()
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
        let mirror_count = state.draft.iter().filter(|output| output.enabled).count();
        self.button(
            panel.primary,
            if primary {
                "Primary: yes"
            } else {
                "Set primary"
            },
            primary,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.enabled,
            if enabled { "Enabled: yes" } else { "Disabled" },
            enabled,
            selected.is_some(),
            self.palette.surface,
        )?;
        self.button(
            panel.rotate,
            &format!(
                "Rotate: {}",
                selected
                    .map(|output| output.rotation.as_str())
                    .unwrap_or("normal")
            ),
            false,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.identify,
            "Identify",
            false,
            true,
            self.palette.surface,
        )?;
        self.button(
            panel.mode_prev,
            "Mode -",
            false,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.mode_next,
            "Mode +",
            false,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.rate_prev,
            "Rate -",
            false,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.rate_next,
            "Rate +",
            false,
            enabled,
            self.palette.surface,
        )?;
        self.button(
            panel.single,
            "Single",
            false,
            selected.is_some(),
            self.palette.surface,
        )?;
        self.button(
            panel.extend,
            "Extend",
            false,
            state.draft.len() > 1,
            self.palette.surface,
        )?;
        self.button(
            panel.mirror,
            "Mirror",
            false,
            mirror_count > 1 && enabled,
            self.palette.surface,
        )?;
        if let Some(output) = selected {
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
        let changed = state.preview_changed();
        self.button(
            panel.apply[0],
            "Preview",
            false,
            !state.busy,
            self.palette.surface,
        )?;
        self.button(
            panel.apply[1],
            &format!(
                "Keep{}",
                state
                    .status
                    .preview
                    .as_ref()
                    .map(|preview| format!(" ({}s)", preview.seconds_left))
                    .unwrap_or_default()
            ),
            state.preview_active(),
            state.preview_active() && !changed && !state.busy,
            self.palette.surface,
        )?;
        self.button(
            panel.apply[2],
            "Revert",
            false,
            state.preview_active() && !state.busy,
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
            let selected = state.selected_profile == Some(*index);
            let startup = state.status.startup_profile.as_deref() == Some(profile.name.as_str());
            let mut label = format!("{} ({})", profile.name, profile.outputs.len());
            if startup {
                label.push_str(" · login");
            }
            self.button(*rect, &label, selected, true, self.palette.surface)?;
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
            (0, "Save as…", false),
            (1, "Rename", false),
            (2, "Delete", false),
            (3, startup_text, startup_selected),
            (4, "Reload", false),
        ] {
            let enabled = match index {
                1..=3 => state.selected_profile.is_some(),
                _ => true,
            };
            self.button(
                layout.actions[index],
                text,
                active && enabled,
                enabled && !state.busy,
                self.palette.surface,
            )?;
        }
        Ok(())
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
        let exists = !state.name_text.trim().is_empty()
            && state
                .status
                .profiles
                .iter()
                .any(|profile| profile.name == state.name_text.trim());
        let hint = if exists {
            "Enter replaces the existing profile · Esc cancels"
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
        let rect = confirm_rect(self.w, self.h);
        self.fill(self.palette.background, rect)?;
        self.outline(rect, 2, self.palette.error())?;
        let (title, body) = match &state.confirm {
            Some(Confirm::Close) => (
                "Close with unsaved changes?",
                "The draft has not been previewed and kept.",
            ),
            Some(Confirm::Delete(name)) => ("Delete display profile?", name.as_str()),
            None => ("", ""),
        };
        self.label(
            rect.x + 16,
            rect.y + 14,
            title,
            self.palette.foreground,
            14.0,
        )?;
        self.label_fit(
            rect.x + 16,
            rect.y + 46,
            body,
            self.palette.muted,
            self.palette.background,
            12.0,
            f32::from(rect.width) - 32.0,
        )?;
        let (yes, no) = confirm_buttons(self.w, self.h);
        let yes_label = if matches!(state.confirm, Some(Confirm::Close)) {
            "Close"
        } else {
            "Delete"
        };
        self.button(yes, yes_label, true, true, self.palette.surface)?;
        self.button(no, "Cancel", false, true, self.palette.surface)?;
        self.label(
            rect.x + 16,
            rect.y + i16::try_from(rect.height).unwrap_or(0) - 64,
            "Enter confirms · Esc cancels",
            self.palette.muted,
            11.0,
        )?;
        Ok(())
    }

    fn paint_help(&self) -> Result<(), String> {
        let rect = Rectangle {
            x: 40,
            y: 80,
            width: self.w.saturating_sub(80),
            height: self.h.saturating_sub(160),
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
            "Esc/Q        close; an active preview reverts on close or timeout",
            "Profiles     click to load, then Preview; Save as… stores the draft",
            "Preview      changes stay for 15 s; Keep confirms, Revert restores",
            "Screens      drag the < > buttons or use the wheel to page",
            "Canvas       wheel zooms; middle-drag pans; 0 resets the view",
        ];
        for (index, line) in lines.iter().enumerate() {
            self.label_fit(
                rect.x + 20,
                rect.y + 58 + index as i16 * 22,
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
        let warning = issues.iter().find(|issue| !issue.blocking);
        let (message, colour) = if state.busy {
            ("Applying…".to_string(), self.palette.muted)
        } else if state.message_is_error {
            (state.message.clone(), self.palette.error())
        } else if let Some(issue) = blocking {
            (issue.text.clone(), self.palette.error())
        } else if !state.preview_active() {
            if let Some(issue) = warning {
                (issue.text.clone(), self.palette.warning())
            } else {
                (state.message.clone(), self.palette.muted)
            }
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
                "Tab focus · drag screens · arrows move · I identify · Ctrl+Z undo · Enter preview · F1 help · Esc close",
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

    fn refresh(&self, state: &State, ui: &Ui, help: bool) -> Result<(), String> {
        self.paint(state, ui, help)?;
        self.flush()
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

fn wait_for_input(app: &App) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let fd = app.c.stream().as_raw_fd();
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd describing the live X socket is passed.
    let _ = unsafe { libc::poll(&mut poll, 1, 250) };
    Ok(())
}

pub fn run(app: &mut App, state: &mut State) -> Result<(), String> {
    let mut help = false;
    let mut dirty = true;
    let mut last_refresh = Instant::now();
    let mut last_theme = Instant::now();
    'outer: loop {
        let mut handled = false;
        while let Some(event) = app.c.poll_for_event().map_err(|e| e.to_string())? {
            handled = true;
            if !matches!(event, Event::NoExposure(_)) {
                dirty = true;
            }
            match event {
                Event::Expose(e) if e.count != 0 => {}
                Event::ConfigureNotify(e) => {
                    if e.width != app.w || e.height != app.h {
                        app.w = e.width.max(MIN_WIDTH);
                        app.h = e.height.max(MIN_HEIGHT);
                        app.resize_backing()?;
                    }
                }
                Event::KeyPress(e) => {
                    let key = app.keys.sym(e.detail);
                    let shift = e.state.contains(KeyButMask::SHIFT);
                    let ctrl = e.state.contains(KeyButMask::CONTROL);
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
                                    Some(NameAction::Save) => {
                                        let ui =
                                            Ui::new(state, app.w, app.h, None, app.zoom, app.pan);
                                        state.busy = true;
                                        let _ = app.refresh(state, &ui, help);
                                        let result = state.save(&text);
                                        state.busy = false;
                                        result
                                    }
                                    Some(NameAction::Rename) => state.rename(&text),
                                    None => Ok(()),
                                };
                                if let Err(err) = result {
                                    state.set_message(err, true);
                                }
                                dirty = true;
                            }
                            _ => {
                                if let Some(ch) = text_key(key, shift) {
                                    if state.name_text.chars().count() < 64 {
                                        state.name_text.push(ch);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    if state.confirm.is_some() {
                        match key {
                            K_ESCAPE | K_N | K_N_SHIFTED => {
                                state.confirm = None;
                            }
                            K_RETURN | K_Y | K_Y_SHIFTED => {
                                confirm_action(app, state, help)?;
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if help {
                        help = false;
                        continue;
                    }
                    if key == K_ESCAPE {
                        if app.drag.is_some() {
                            cancel_drag(app, state);
                        } else if state.focus.is_some() {
                            state.focus = None;
                        } else {
                            request_close(state);
                        }
                        continue;
                    }
                    if key == K_Q || key == K_Q_SHIFTED {
                        request_close(state);
                        continue;
                    }
                    if key == K_F1 {
                        help = true;
                        continue;
                    }
                    if ctrl && key == K_Z {
                        state.undo();
                        continue;
                    }
                    if key == K_I && !ctrl {
                        if let Err(err) = app.identify(state) {
                            state.set_message(err, true);
                        }
                        continue;
                    }
                    if key == K_R && !ctrl {
                        if let Err(err) = state.reload(true) {
                            state.set_message(err, true);
                        }
                        dirty = true;
                        continue;
                    }
                    if key == K_0 && !ctrl {
                        app.zoom = 1.0;
                        app.pan = (0.0, 0.0);
                        state.set_message("Canvas zoom reset.", false);
                        continue;
                    }
                    if key == K_TAB {
                        let ui = Ui::new(
                            state,
                            app.w,
                            app.h,
                            app.drag.as_ref().map(|drag| drag.canvas),
                            app.zoom,
                            app.pan,
                        );
                        let order = focus_order(&ui);
                        if order.is_empty() {
                            continue;
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
                        continue;
                    }
                    if matches!(key, K_PAGEUP | K_PAGEDOWN) {
                        let ui = Ui::new(
                            state,
                            app.w,
                            app.h,
                            app.drag.as_ref().map(|drag| drag.canvas),
                            app.zoom,
                            app.pan,
                        );
                        page_outputs(state, &ui, if key == K_PAGEUP { -1 } else { 1 });
                        continue;
                    }
                    if matches!(key, K_LEFT | K_RIGHT | K_UP | K_DOWN) {
                        let step = if shift { 10 } else { 1 };
                        let ui = Ui::new(
                            state,
                            app.w,
                            app.h,
                            app.drag.as_ref().map(|drag| drag.canvas),
                            app.zoom,
                            app.pan,
                        );
                        handle_arrows(state, &ui, key, step);
                        continue;
                    }
                    if key == K_RETURN || key == K_SPACE {
                        if let Some(focus) = state.focus {
                            let ui = Ui::new(
                                state,
                                app.w,
                                app.h,
                                app.drag.as_ref().map(|drag| drag.canvas),
                                app.zoom,
                                app.pan,
                            );
                            activate(app, state, &ui, focus, help)?;
                        } else if key == K_RETURN {
                            let ui = Ui::new(state, app.w, app.h, None, app.zoom, app.pan);
                            state.busy = true;
                            let _ = app.refresh(state, &ui, help);
                            if let Err(err) = state.apply_preview() {
                                state.set_message(err, true);
                            }
                            state.busy = false;
                        }
                        continue;
                    }
                }
                Event::ButtonPress(e) => {
                    if state.name_mode.is_some() {
                        continue;
                    }
                    if state.confirm.is_some() {
                        let (yes, no) = confirm_buttons(app.w, app.h);
                        if e.detail == 1 && contains(yes, e.event_x, e.event_y) {
                            confirm_action(app, state, help)?;
                        } else if e.detail == 1 && contains(no, e.event_x, e.event_y) {
                            state.confirm = None;
                        }
                        continue;
                    }
                    let ui = Ui::new(
                        state,
                        app.w,
                        app.h,
                        app.drag.as_ref().map(|drag| drag.canvas),
                        app.zoom,
                        app.pan,
                    );
                    if e.detail == 4 || e.detail == 5 {
                        if contains(ui.l.profiles, e.event_x, e.event_y) {
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
                        continue;
                    }
                    if e.detail == 2 && contains(ui.l.canvas, e.event_x, e.event_y) {
                        app.pan_drag = Some((e.event_x, e.event_y, app.pan.0, app.pan.1));
                        continue;
                    }
                    state.focus = None;
                    if e.detail != 1 {
                        continue;
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
                        continue;
                    }
                    if let Some(action) = hit(state, &ui, e.event_x, e.event_y) {
                        activate(app, state, &ui, action, help)?;
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
                        dirty = true;
                        continue;
                    }
                    if app.drag.is_some() {
                        if !e.state.contains(KeyButMask::BUTTON1) {
                            finish_drag(app, state);
                            continue;
                        }
                        let mouse = (e.event_x, e.event_y);
                        drag_motion(state, app, mouse);
                        continue;
                    }
                    let ui = Ui::new(state, app.w, app.h, None, app.zoom, app.pan);
                    let hover = hit(state, &ui, e.event_x, e.event_y);
                    if hover != app.hover {
                        app.hover = hover;
                        let cursor = app.cursors.map(|cursors| match hover {
                            Some(Hit::Monitor(_)) => cursors.fleur,
                            Some(Hit::Output(_))
                            | Some(Hit::OutputPrev)
                            | Some(Hit::OutputNext)
                            | Some(Hit::Primary)
                            | Some(Hit::Enabled)
                            | Some(Hit::Rotate)
                            | Some(Hit::Identify)
                            | Some(Hit::ModePrev)
                            | Some(Hit::ModeNext)
                            | Some(Hit::RatePrev)
                            | Some(Hit::RateNext)
                            | Some(Hit::Single)
                            | Some(Hit::Extend)
                            | Some(Hit::Mirror)
                            | Some(Hit::Apply)
                            | Some(Hit::Keep)
                            | Some(Hit::Revert)
                            | Some(Hit::Save)
                            | Some(Hit::Rename)
                            | Some(Hit::Delete)
                            | Some(Hit::Startup)
                            | Some(Hit::Reload)
                            | Some(Hit::Profile(_))
                            | Some(Hit::ProfilePrev)
                            | Some(Hit::ProfileNext) => cursors.hand,
                            _ => cursors.normal,
                        });
                        app.set_cursor(cursor)?;
                        dirty = true;
                    }
                }
                Event::ClientMessage(e) => {
                    let close = app
                        .c
                        .intern_atom(false, b"WM_PROTOCOLS")
                        .ok()
                        .and_then(|cookie| cookie.reply().ok())
                        .map(|reply| reply.atom);
                    if Some(e.type_) == close {
                        request_close(state);
                    }
                }
                _ => {}
            }
        }
        if state.quit {
            break 'outer;
        }
        if let Some(until) = app.identify_until {
            if Instant::now() >= until {
                let _ = app.clear_identify();
            }
        }
        let interval = if state.preview_active() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(2)
        };
        if last_refresh.elapsed() >= interval
            && state.name_mode.is_none()
            && state.confirm.is_none()
            && app.drag.is_none()
        {
            last_refresh = Instant::now();
            if let Err(err) = state.reload(false) {
                state.set_message(err, true);
            }
            dirty = true;
        }
        if dirty {
            if last_theme.elapsed() >= Duration::from_millis(750) {
                app.palette = theme::load();
                last_theme = Instant::now();
            }
            let ui = Ui::new(
                state,
                app.w,
                app.h,
                app.drag.as_ref().map(|drag| drag.canvas),
                app.zoom,
                app.pan,
            );
            app.paint(state, &ui, help)?;
            app.flush()?;
            dirty = false;
        } else {
            app.c.flush().map_err(|e| e.to_string())?;
        }
        if !handled {
            wait_for_input(app)?;
        }
    }
    if state.preview_active() {
        let _ = backend::revert();
    }
    Ok(())
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
    if state.dirty {
        state.confirm = Some(Confirm::Close);
    } else {
        state.quit = true;
    }
}

fn confirm_action(app: &mut App, state: &mut State, help: bool) -> Result<(), String> {
    let Some(confirm) = state.confirm.take() else {
        return Ok(());
    };
    match confirm {
        Confirm::Close => {
            state.quit = true;
        }
        Confirm::Delete(name) => {
            let ui = Ui::new(state, app.w, app.h, None, app.zoom, app.pan);
            state.busy = true;
            let _ = app.refresh(state, &ui, help);
            if let Err(err) = state.delete_named(&name) {
                state.set_message(err, true);
            }
            state.busy = false;
        }
    }
    Ok(())
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
    let pages = ui.profiles.pages as i32;
    let page = state.profile_scroll as i32 + direction;
    state.profile_scroll = page.clamp(0, pages.saturating_sub(1)) as usize;
}

fn handle_arrows(state: &mut State, ui: &Ui, key: u32, step: i32) {
    match state.focus {
        Some(Hit::ModePrev) | Some(Hit::ModeNext) => {
            let delta = if matches!(key, K_LEFT | K_UP) { -1 } else { 1 };
            state.cycle_mode(delta);
        }
        Some(Hit::RatePrev) | Some(Hit::RateNext) => {
            let delta = if matches!(key, K_LEFT | K_UP) { -1 } else { 1 };
            state.cycle_rate(delta);
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

fn activate(
    app: &mut App,
    state: &mut State,
    ui: &Ui,
    action: Hit,
    help: bool,
) -> Result<(), String> {
    let blocking = matches!(
        action,
        Hit::Apply
            | Hit::Keep
            | Hit::Revert
            | Hit::Save
            | Hit::Rename
            | Hit::Delete
            | Hit::Startup
            | Hit::Reload
    );
    if blocking {
        state.busy = true;
        let _ = app.refresh(state, ui, help);
    }
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
        Hit::Profile(index) => state.load_profile(index),
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
        Hit::Rotate => {
            state.rotate();
            Ok(())
        }
        Hit::ModePrev => {
            state.cycle_mode(-1);
            Ok(())
        }
        Hit::ModeNext => {
            state.cycle_mode(1);
            Ok(())
        }
        Hit::RatePrev => {
            state.cycle_rate(-1);
            Ok(())
        }
        Hit::RateNext => {
            state.cycle_rate(1);
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
            state.name_text = state
                .selected_profile
                .and_then(|index| state.status.profiles.get(index))
                .map(|profile| profile.name.clone())
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
        Hit::Startup => {
            let name = state
                .selected_profile
                .and_then(|index| state.status.profiles.get(index))
                .map(|profile| profile.name.clone());
            if let Some(name) = name {
                let next = if state.status.startup_profile.as_deref() == Some(name.as_str()) {
                    None
                } else {
                    Some(name.as_str())
                };
                match backend::set_startup(next) {
                    Ok(()) => {
                        let _ = state.reload(false);
                        state.set_message(
                            if next.is_some() {
                                "This profile will be applied at the next login."
                            } else {
                                "Startup profile disabled."
                            },
                            false,
                        );
                    }
                    Err(err) => state.set_message(err, true),
                }
            } else {
                state.set_message("Select a saved profile first.", true);
            }
            Ok(())
        }
        Hit::Reload => state.reload(true),
        Hit::NameField => Ok(()),
    };
    if let Err(err) = result {
        state.set_message(err, true);
    }
    if blocking {
        state.busy = false;
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
        State {
            status: backend::Status::default(),
            draft,
            selected: 0,
            selected_profile: None,
            focus: None,
            message: String::new(),
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            quit: false,
            had_preview: false,
            preview_layout: None,
            undo: Vec::new(),
        }
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
        let mut state = State {
            status: backend::Status::default(),
            draft: (0..6)
                .map(|index| output(&format!("o{index}"), index * 1920, 0, "1920x1080"))
                .collect(),
            selected: 0,
            selected_profile: None,
            focus: None,
            message: String::new(),
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            quit: false,
            had_preview: false,
            preview_layout: None,
            undo: Vec::new(),
        };
        state.draft[0].primary = true;
        let ui = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        let mut rects = vec![
            ui.panel.primary,
            ui.panel.enabled,
            ui.panel.rotate,
            ui.panel.identify,
            ui.panel.mode_prev,
            ui.panel.mode_next,
            ui.panel.rate_prev,
            ui.panel.rate_next,
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
        let mut state = State {
            status: backend::Status::default(),
            draft: vec![
                output("a", 0, 0, "1920x1080"),
                output("b", 1920, 0, "1920x1080"),
            ],
            selected: 0,
            selected_profile: None,
            focus: None,
            message: String::new(),
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            quit: false,
            had_preview: false,
            preview_layout: None,
            undo: Vec::new(),
        };
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
            });
        }
        state.status.profiles = profiles;
        let first = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert!(first.panel.rows.len() < 8);
        assert!(first.panel.page_next.is_some());
        assert_eq!(first.profiles.rows.len(), 8);
        assert!(first.profiles.next.is_some());
        state.output_scroll = 2;
        state.profile_scroll = 1;
        let second = Ui::new(&state, MIN_WIDTH, MIN_HEIGHT, None, 1.0, (0.0, 0.0));
        assert_eq!(second.panel.rows.first().map(|(index, _)| *index), Some(2));
        assert_eq!(
            second.profiles.rows.first().map(|(index, _)| *index),
            Some(8)
        );
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
    fn dragging_a_mirror_group_moves_both_members() {
        let mut state = State {
            status: backend::Status::default(),
            draft: vec![
                output("a", 0, 0, "1920x1080"),
                output("b", 0, 0, "1920x1080"),
                output("c", 1920, 0, "1920x1080"),
            ],
            selected: 0,
            selected_profile: None,
            focus: None,
            message: String::new(),
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            output_scroll: 0,
            profile_scroll: 0,
            confirm: None,
            quit: false,
            had_preview: false,
            preview_layout: None,
            undo: Vec::new(),
        };
        state.draft[0].primary = true;
        state.move_selected(10, 5);
        assert_eq!((state.draft[0].x, state.draft[0].y), (10, 5));
        assert_eq!((state.draft[1].x, state.draft[1].y), (10, 5));
        assert_eq!((state.draft[2].x, state.draft[2].y), (1920, 0));
    }
}
