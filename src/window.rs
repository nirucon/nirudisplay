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
const MIN_HEIGHT: u16 = 620;

const PAD: i16 = 20;
const HEADER: i16 = 76;
const FOOTER: i16 = 56;
const PANEL_W: i16 = 330;
const PROFILE_H: i16 = 132;
const ROW_H: i16 = 32;

const K_ESCAPE: u32 = 0xff1b;
const K_TAB: u32 = 0xff09;
const K_RETURN: u32 = 0xff0d;
const K_BACKSPACE: u32 = 0xff08;
const K_LEFT: u32 = 0xff51;
const K_UP: u32 = 0xff52;
const K_RIGHT: u32 = 0xff53;
const K_DOWN: u32 = 0xff54;
const K_SPACE: u32 = 0x0020;
const K_F1: u32 = 0xffbe;
const K_Q: u32 = b'q' as u32;
const K_Q_SHIFTED: u32 = b'Q' as u32;
const K_I: u32 = b'i' as u32;
const K_R: u32 = b'r' as u32;

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
    Mirror,
    Apply,
    Keep,
    Revert,
    Save,
    Rename,
    Delete,
    Startup,
    Reload,
    Profile(usize),
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
        let (mut width, mut height) = self
            .mode
            .split_once('x')
            .and_then(|(w, rest)| {
                let h = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("");
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((1920, 1080));
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameAction {
    Save,
    Rename,
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
    pub identify_until: Option<Instant>,
    had_preview: bool,
}

impl State {
    pub fn load() -> Result<Self, String> {
        let status = backend::read_status()?;
        let draft = draft_from_status(&status);
        Ok(Self {
            status,
            draft,
            selected: 0,
            selected_profile: None,
            focus: None,
            message: "Drag a screen to move it. Changes are previewed before they are kept.".into(),
            message_is_error: false,
            busy: false,
            dirty: false,
            name_mode: None,
            name_text: String::new(),
            identify_until: None,
            had_preview: false,
        })
    }

    pub fn set_message(&mut self, text: impl Into<String>, error: bool) {
        self.message = text.into();
        self.message_is_error = error;
    }

    pub fn current(&self) -> Option<&DraftOutput> {
        self.draft.get(self.selected)
    }

    pub fn current_mut(&mut self) -> Option<&mut DraftOutput> {
        self.draft.get_mut(self.selected)
    }

    pub fn preview_active(&self) -> bool {
        self.status.preview.is_some()
    }

    pub fn reload(&mut self, force_draft: bool) -> Result<(), String> {
        let status = backend::read_status()?;
        if force_draft || !self.dirty {
            self.draft = draft_from_status(&status);
            if self.selected >= self.draft.len() {
                self.selected = self.draft.len().saturating_sub(1);
            }
            self.dirty = false;
        }
        if let Some(preview) = &status.preview {
            self.had_preview = true;
            self.message = format!(
                "Previewing {}. Keep within {} s or the previous layout returns.",
                preview.name, preview.seconds_left
            );
            self.message_is_error = false;
        } else if self.had_preview {
            self.had_preview = false;
            if self.dirty {
                self.draft = draft_from_status(&status);
                self.dirty = false;
            }
            self.message = "The preview ended; the previous layout is back.".into();
            self.message_is_error = false;
        }
        self.status = status;
        Ok(())
    }

    fn set_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn move_selected(&mut self, dx: i32, dy: i32) {
        if let Some(output) = self.current_mut() {
            if output.enabled {
                output.x += dx;
                output.y += dy;
                self.set_dirty();
            }
        }
    }

    pub fn toggle_enabled(&mut self) {
        let Some(mut next) = self.current().cloned() else {
            return;
        };
        if next.enabled && self.draft.iter().filter(|output| output.enabled).count() == 1 {
            self.set_message("At least one screen must stay enabled.", true);
            return;
        }
        next.enabled = !next.enabled;
        if next.enabled && next.x == 0 && next.y == 0 {
            let right = self
                .draft
                .iter()
                .filter(|output| output.enabled && output.name != next.name)
                .map(|output| output.x + output.width())
                .max()
                .unwrap_or(0);
            next.x = right;
        }
        self.draft[self.selected] = next;
        self.set_dirty();
    }

    pub fn set_primary(&mut self) {
        let Some(selected) = self.current().map(|output| output.name.clone()) else {
            return;
        };
        for output in &mut self.draft {
            output.primary = output.name == selected && output.enabled;
        }
        self.set_dirty();
    }

    pub fn rotate(&mut self) {
        if let Some(output) = self.current_mut() {
            output.rotation = match output.rotation.as_str() {
                "normal" => "left",
                "left" => "inverted",
                "inverted" => "right",
                _ => "normal",
            }
            .into();
            self.set_dirty();
        }
    }

    pub fn mirror_screens(&mut self) {
        let Some(source) = self.current().cloned() else {
            return;
        };
        if self.draft.iter().filter(|output| output.enabled).count() < 2 {
            self.set_message("Enable at least two screens to mirror them.", true);
            return;
        }
        for output in &mut self.draft {
            if !output.enabled {
                continue;
            }
            output.mode = source.mode.clone();
            output.rate = source.rate;
            output.rotation = source.rotation.clone();
            output.x = 0;
            output.y = 0;
            output.primary = output.name == source.name;
        }
        self.set_dirty();
        self.set_message(
            "Mirroring the selected screen on every enabled output. Preview to confirm.",
            false,
        );
    }

    fn mode_index(&self) -> Option<usize> {
        let output = self.current()?;
        let state = self.status.output(&output.name)?;
        state.modes.iter().position(|mode| mode.mode == output.mode)
    }

    pub fn cycle_mode(&mut self, delta: i32) {
        let Some(index) = self.mode_index() else {
            self.set_message("No available modes for this screen.", true);
            return;
        };
        let name = self
            .current()
            .map(|output| output.name.clone())
            .unwrap_or_default();
        let next = self.status.output(&name).and_then(|state| {
            if state.modes.is_empty() {
                return None;
            }
            let next = (index as i32 + delta).rem_euclid(state.modes.len() as i32) as usize;
            state
                .modes
                .get(next)
                .map(|mode| (mode.mode.clone(), mode.rates.first().copied()))
        });
        if let (Some((mode, rate)), Some(output)) = (next, self.current_mut()) {
            output.mode = mode;
            output.rate = rate;
            self.set_dirty();
        }
    }

    pub fn cycle_rate(&mut self, delta: i32) {
        let Some(output) = self.current().cloned() else {
            return;
        };
        let next = self.status.output(&output.name).and_then(|state| {
            let mode = state.modes.iter().find(|mode| mode.mode == output.mode)?;
            if mode.rates.is_empty() {
                return None;
            }
            let index = output
                .rate
                .and_then(|rate| {
                    mode.rates
                        .iter()
                        .position(|candidate| (candidate - rate).abs() < 0.01)
                })
                .unwrap_or(0) as i32;
            let next = (index + delta).rem_euclid(mode.rates.len() as i32) as usize;
            mode.rates.get(next).copied()
        });
        if let (Some(rate), Some(output)) = (next, self.current_mut()) {
            output.rate = Some(rate);
            self.set_dirty();
        }
    }

    pub fn profile_from_draft(&self) -> backend::Profile {
        backend::Profile {
            name: self
                .selected_profile
                .and_then(|index| self.status.profiles.get(index))
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "Draft".into()),
            outputs: self
                .draft
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
        let mut draft = self.draft.clone();
        for output in &mut draft {
            output.enabled = false;
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
        self.draft = draft;
        self.selected = self
            .draft
            .iter()
            .position(|output| output.enabled)
            .unwrap_or(0);
        self.selected_profile = Some(index);
        self.dirty = true;
        self.set_message(
            format!("Loaded {}. Apply to preview it.", profile.name),
            false,
        );
        Ok(())
    }

    pub fn apply_preview(&mut self) -> Result<(), String> {
        if self.draft.iter().filter(|output| output.enabled).count() == 0 {
            return Err("at least one screen must be enabled".into());
        }
        let profile = self.profile_from_draft();
        backend::preview(&profile, 15)?;
        self.dirty = true;
        self.had_preview = true;
        self.set_message(
            "Previewing for 15 s. Keep the result or let it revert.",
            false,
        );
        let _ = self.reload(false);
        Ok(())
    }

    pub fn keep(&mut self) -> Result<(), String> {
        backend::keep()?;
        self.dirty = false;
        self.had_preview = false;
        self.set_message("Display layout kept.", false);
        let _ = self.reload(true);
        Ok(())
    }

    pub fn revert(&mut self) -> Result<(), String> {
        backend::revert()?;
        self.dirty = false;
        self.had_preview = false;
        self.set_message("Previous layout restored.", false);
        let _ = self.reload(true);
        Ok(())
    }

    pub fn save(&mut self, name: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("a profile name is required".into());
        }
        let mut profile = self.profile_from_draft();
        profile.name = name.trim().to_string();
        backend::save_profile(&profile, name.trim())?;
        self.dirty = false;
        self.set_message(format!("Saved display profile {}.", name.trim()), false);
        self.reload(true)?;
        self.selected_profile = self
            .status
            .profiles
            .iter()
            .position(|profile| profile.name == name.trim());
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
        self.reload(false)?;
        self.selected_profile = self
            .status
            .profiles
            .iter()
            .position(|profile| profile.name == new.trim());
        self.set_message(format!("Renamed {old} to {}.", new.trim()), false);
        Ok(())
    }

    pub fn delete(&mut self) -> Result<(), String> {
        let Some(index) = self.selected_profile else {
            return Err("select a saved profile first".into());
        };
        let name = self
            .status
            .profiles
            .get(index)
            .map(|profile| profile.name.clone())
            .ok_or("unknown display profile")?;
        backend::delete_profile(&name)?;
        self.reload(false)?;
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

fn active_bounds(draft: &[DraftOutput]) -> (i32, i32, i32, i32) {
    let active: Vec<&DraftOutput> = draft.iter().filter(|output| output.enabled).collect();
    if active.is_empty() {
        return (0, 0, 1920, 1080);
    }
    let min_x = active.iter().map(|output| output.x).min().unwrap_or(0);
    let min_y = active.iter().map(|output| output.y).min().unwrap_or(0);
    let max_x = active
        .iter()
        .map(|output| output.x + output.width())
        .max()
        .unwrap_or(1920);
    let max_y = active
        .iter()
        .map(|output| output.y + output.height())
        .max()
        .unwrap_or(1080);
    (min_x, min_y, (max_x - min_x).max(1), (max_y - min_y).max(1))
}

fn canvas_for(area: Rectangle, draft: &[DraftOutput]) -> Canvas {
    let (min_x, min_y, width, height) = active_bounds(draft);
    let margin = 30.0;
    let avail_w = (f32::from(area.width) - margin * 2.0).max(1.0);
    let avail_h = (f32::from(area.height) - margin * 2.0).max(1.0);
    let scale = (avail_w / width as f32)
        .min(avail_h / height as f32)
        .clamp(0.02, 1.0);
    let drawn_w = width as f32 * scale;
    let drawn_h = height as f32 * scale;
    Canvas {
        x: f32::from(area.x) + (f32::from(area.width) - drawn_w) / 2.0 - min_x as f32 * scale,
        y: f32::from(area.y) + (f32::from(area.height) - drawn_h) / 2.0 - min_y as f32 * scale,
        scale,
    }
}

fn monitor_rect(canvas: Canvas, output: &DraftOutput) -> Rectangle {
    Rectangle {
        x: (canvas.x + output.x as f32 * canvas.scale).round() as i16,
        y: (canvas.y + output.y as f32 * canvas.scale).round() as i16,
        width: (output.width() as f32 * canvas.scale).round().max(8.0) as u16,
        height: (output.height() as f32 * canvas.scale).round().max(8.0) as u16,
    }
}

fn output_row(l: Layout, index: usize) -> Rectangle {
    Rectangle {
        x: l.panel.x + 10,
        y: l.panel.y + 34 + index as i16 * ROW_H,
        width: l.panel.width.saturating_sub(20),
        height: ROW_H as u16 - 2,
    }
}

fn panel_button(l: Layout, slot: i16) -> Rectangle {
    Rectangle {
        x: l.panel.x + 10,
        y: l.panel.y + 224 + slot * 30,
        width: l.panel.width.saturating_sub(20),
        height: 26,
    }
}

fn pair_button(l: Layout, slot: i16, right: bool) -> Rectangle {
    let width = (l.panel.width.saturating_sub(26)) / 2;
    Rectangle {
        x: l.panel.x
            + 10
            + if right {
                i16::try_from(width).unwrap_or(0) + 6
            } else {
                0
            },
        y: l.panel.y + 224 + slot * 30,
        width,
        height: 26,
    }
}

fn apply_buttons(l: Layout) -> [Rectangle; 3] {
    let width = (l.panel.width.saturating_sub(32)) / 3;
    [
        Rectangle {
            x: l.panel.x + 10,
            y: l.panel.y + i16::try_from(l.panel.height).unwrap_or(0) - 36,
            width,
            height: 26,
        },
        Rectangle {
            x: l.panel.x + 10 + i16::try_from(width).unwrap_or(0) + 6,
            y: l.panel.y + i16::try_from(l.panel.height).unwrap_or(0) - 36,
            width,
            height: 26,
        },
        Rectangle {
            x: l.panel.x + 10 + i16::try_from(width).unwrap_or(0) * 2 + 12,
            y: l.panel.y + i16::try_from(l.panel.height).unwrap_or(0) - 36,
            width,
            height: 26,
        },
    ]
}

fn profile_rect(l: Layout, index: usize) -> Rectangle {
    let columns = 4usize;
    let column = index % columns;
    let row = index / columns;
    let width = (l.profiles.width.saturating_sub(18)) / columns as u16;
    Rectangle {
        x: l.profiles.x + column as i16 * i16::try_from(width + 4).unwrap_or(0),
        y: l.profiles.y + 30 + row as i16 * 31,
        width,
        height: 27,
    }
}

fn action_rect(l: Layout, index: usize) -> Rectangle {
    let width = (l.profiles.width.saturating_sub(24)) / 5;
    Rectangle {
        x: l.profiles.x + index as i16 * i16::try_from(width + 4).unwrap_or(0),
        y: l.profiles.y + i16::try_from(l.profiles.height).unwrap_or(0) - 33,
        width,
        height: 27,
    }
}

fn name_rect(w: u16, h: u16) -> Rectangle {
    let width = 520.min(w.saturating_sub(80));
    let height = 120u16;
    Rectangle {
        x: (i16::try_from(w).unwrap_or(i16::MAX) - i16::try_from(width).unwrap_or(0)) / 2,
        y: (i16::try_from(h).unwrap_or(i16::MAX) - height as i16) / 2,
        width,
        height,
    }
}

fn hit(state: &State, l: Layout, canvas: Canvas, x: i16, y: i16) -> Option<Hit> {
    if state.name_mode.is_some() {
        return Some(Hit::NameField);
    }
    for (index, output) in state.draft.iter().enumerate().rev() {
        if !output.enabled {
            continue;
        }
        let rect = monitor_rect(canvas, output);
        if x >= rect.x
            && x < rect.x + i16::try_from(rect.width).unwrap_or(0)
            && y >= rect.y
            && y < rect.y + i16::try_from(rect.height).unwrap_or(0)
        {
            return Some(Hit::Monitor(index));
        }
    }
    for index in 0..state.draft.len().min(5) {
        let rect = output_row(l, index);
        if contains(rect, x, y) {
            return Some(Hit::Output(index));
        }
    }
    for (slot, hit) in [
        (0, Hit::Primary),
        (1, Hit::Enabled),
        (2, Hit::Rotate),
        (3, Hit::ModePrev),
        (3, Hit::ModeNext),
        (4, Hit::RatePrev),
        (4, Hit::RateNext),
        (5, Hit::Identify),
        (5, Hit::Mirror),
    ] {
        let rect = if matches!(hit, Hit::ModeNext | Hit::RateNext | Hit::Mirror) {
            pair_button(l, slot, true)
        } else if matches!(hit, Hit::ModePrev | Hit::RatePrev | Hit::Identify) {
            pair_button(l, slot, false)
        } else {
            panel_button(l, slot)
        };
        if contains(rect, x, y) {
            return Some(hit);
        }
    }
    for (index, hit) in [Hit::Apply, Hit::Keep, Hit::Revert].into_iter().enumerate() {
        if contains(apply_buttons(l)[index], x, y) {
            return Some(hit);
        }
    }
    for index in 0..state.status.profiles.len().min(8) {
        if contains(profile_rect(l, index), x, y) {
            return Some(Hit::Profile(index));
        }
    }
    for (index, hit) in [
        Hit::Save,
        Hit::Rename,
        Hit::Delete,
        Hit::Startup,
        Hit::Reload,
    ]
    .into_iter()
    .enumerate()
    {
        if contains(action_rect(l, index), x, y) {
            return Some(hit);
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
    drag: Option<(usize, i16, i16, i32, i32)>,
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
        // A settings window should stay a floating, centred dialog instead of
        // being tiled like a terminal. NIRUWM treats this type as floating.
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
        self.c.free_pixmap(old).map_err(|e| e.to_string())?;
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

    fn button(
        &self,
        rect: Rectangle,
        text: &str,
        active: bool,
        enabled: bool,
    ) -> Result<(), String> {
        let (bg, fg) = if !enabled {
            (self.palette.background, self.palette.border)
        } else if active {
            (self.palette.accent, self.palette.background)
        } else {
            (self.palette.surface, self.palette.foreground)
        };
        self.fill(bg, rect)?;
        self.fill(
            self.palette.border,
            Rectangle {
                width: rect.width,
                height: 1,
                ..rect
            },
        )?;
        let (_, text_h, _) = self.font.size(13.0).render(text, fg, bg);
        let y = rect.y + (i16::try_from(text_h).unwrap_or(0) - 8).max(0) / 2;
        self.label_on(rect.x + 8, y, text, fg, bg, 13.0)
    }

    fn ring(&self, rect: Rectangle) -> Result<(), String> {
        let outer = Rectangle {
            x: rect.x - 2,
            y: rect.y - 2,
            width: rect.width + 4,
            height: rect.height + 4,
        };
        for edge in [
            Rectangle {
                width: outer.width,
                height: 1,
                ..outer
            },
            Rectangle {
                y: outer.y + i16::try_from(outer.height).unwrap_or(0) - 1,
                width: outer.width,
                height: 1,
                ..outer
            },
            Rectangle {
                width: 1,
                height: outer.height,
                ..outer
            },
            Rectangle {
                x: outer.x + i16::try_from(outer.width).unwrap_or(0) - 1,
                width: 1,
                height: outer.height,
                ..outer
            },
        ] {
            self.fill(self.palette.foreground, edge)?;
        }
        Ok(())
    }

    fn identify(&mut self, state: &State) -> Result<(), String> {
        self.clear_identify()?;
        let active: Vec<&DraftOutput> =
            state.draft.iter().filter(|output| output.enabled).collect();
        for (index, output) in active.iter().enumerate() {
            let (width, height) = (output.width().max(1), output.height().max(1));
            let win = self.c.generate_id().map_err(|e| e.to_string())?;
            let bg = self.palette.accent;
            self.c
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    win,
                    self.root,
                    output.x.clamp(0, i32::from(i16::MAX)) as i16,
                    output.y.clamp(0, i32::from(i16::MAX)) as i16,
                    width.min(u16::MAX as i32) as u16,
                    height.min(u16::MAX as i32) as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .override_redirect(1)
                        .background_pixel(bg),
                )
                .map_err(|e| e.to_string())?;
            self.c.map_window(win).map_err(|e| e.to_string())?;
            let size = (height as f32 / 3.0).clamp(40.0, 180.0);
            let label = (index + 1).to_string();
            let (w, h, pixels) = self
                .font
                .size(size)
                .render(&label, self.palette.background, bg);
            let x = (width - w as i32) / 2;
            let y = (height - h as i32) / 2;
            self.upload_to(
                win,
                x.max(0) as i16,
                y.max(0) as i16,
                w as u16,
                h as u16,
                &pixels,
            )?;
            self.overlays.push(win);
        }
        self.identify_until = Some(Instant::now() + Duration::from_secs(2));
        self.c.flush().map_err(|e| e.to_string())
    }

    fn clear_identify(&mut self) -> Result<(), String> {
        for window in self.overlays.drain(..) {
            let _ = self.c.destroy_window(window);
        }
        self.identify_until = None;
        self.c.flush().map_err(|e| e.to_string())
    }

    fn paint(&self, state: &State, l: Layout, help: bool) -> Result<(), String> {
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
        self.fill(self.palette.surface, l.canvas)?;
        let canvas = canvas_for(l.canvas, &state.draft);
        for (index, output) in state.draft.iter().enumerate() {
            if !output.enabled {
                continue;
            }
            let rect = monitor_rect(canvas, output);
            let selected = index == state.selected;
            self.fill(
                if selected {
                    self.palette.surface
                } else {
                    self.palette.background
                },
                rect,
            )?;
            self.fill(
                if selected || output.primary {
                    self.palette.accent
                } else {
                    self.palette.border
                },
                Rectangle {
                    width: rect.width,
                    height: 2,
                    ..rect
                },
            )?;
            self.label(
                rect.x + 8,
                rect.y + 8,
                &format!("{} {}", index + 1, output.name),
                self.palette.foreground,
                13.0,
            )?;
            self.label(
                rect.x + 8,
                rect.y + 28,
                &format!(
                    "{}  {}  {}{}",
                    output.mode,
                    output
                        .rate
                        .map(|rate| format!("{rate:.2} Hz"))
                        .unwrap_or_else(|| "auto".into()),
                    output.rotation,
                    if output.primary { "  primary" } else { "" }
                ),
                self.palette.muted,
                11.0,
            )?;
        }
        if state.draft.iter().filter(|output| output.enabled).count() == 0 {
            self.label(
                l.canvas.x + 20,
                l.canvas.y + 30,
                "No screen is enabled. Select one in the panel and enable it.",
                self.palette.muted,
                13.0,
            )?;
        }
        self.paint_panel(state, l)?;
        self.paint_profiles(state, l)?;
        if let Some(focus) = state.focus {
            if let Some(rect) = focus_rect(state, l, canvas, focus) {
                self.ring(rect)?;
            }
        }
        if state.name_mode.is_some() {
            self.paint_name(state)?;
        } else if help {
            self.paint_help()?;
        }
        let message = if state.busy {
            "Applying…".to_string()
        } else {
            state.message.clone()
        };
        self.label(
            l.footer.x,
            l.footer.y,
            &truncate(&message, (self.w as usize / 7).saturating_sub(2)),
            if state.message_is_error {
                self.palette.accent
            } else {
                self.palette.muted
            },
            12.0,
        )?;
        self.label(
            l.footer.x,
            l.footer.y + 20,
            "Tab focus  arrows move  d drag  i identify  Enter apply  F1 help  Esc close",
            self.palette.muted,
            11.0,
        )?;
        Ok(())
    }

    fn paint_panel(&self, state: &State, l: Layout) -> Result<(), String> {
        self.fill(self.palette.surface, l.panel)?;
        self.label(
            l.panel.x + 10,
            l.panel.y + 8,
            "Screens",
            self.palette.muted,
            12.0,
        )?;
        for (index, output) in state.draft.iter().enumerate().take(5) {
            let rect = output_row(l, index);
            let active = index == state.selected;
            self.fill(
                if active {
                    self.palette.surface
                } else {
                    self.palette.background
                },
                rect,
            )?;
            if active {
                self.fill(
                    self.palette.accent,
                    Rectangle {
                        width: 2,
                        height: rect.height,
                        ..rect
                    },
                )?;
            }
            let detail = if output.enabled {
                format!(
                    "{}  {}{}",
                    output.mode,
                    output
                        .rate
                        .map(|rate| format!("{rate:.2} Hz"))
                        .unwrap_or_else(|| "auto".into()),
                    if output.primary { "  primary" } else { "" }
                )
            } else {
                "disabled".into()
            };
            self.label(
                rect.x + 8,
                rect.y + 2,
                &truncate(&output.name, 28),
                self.palette.foreground,
                12.0,
            )?;
            self.label(
                rect.x + 8,
                rect.y + 15,
                &truncate(&detail, 28),
                self.palette.muted,
                10.0,
            )?;
        }
        let selected = state.current();
        let name = selected
            .map(|output| output.name.as_str())
            .unwrap_or("No screen");
        self.label(
            l.panel.x + 10,
            l.panel.y + 200,
            &format!("Selected: {name}"),
            self.palette.foreground,
            12.0,
        )?;
        let enabled = selected.map(|output| output.enabled).unwrap_or(false);
        self.button(
            panel_button(l, 0),
            if selected.map(|output| output.primary).unwrap_or(false) {
                "Primary screen: yes"
            } else {
                "Set as primary"
            },
            selected.map(|output| output.primary).unwrap_or(false),
            enabled,
        )?;
        self.button(
            panel_button(l, 1),
            if enabled {
                "Screen: enabled"
            } else {
                "Screen: disabled"
            },
            enabled,
            selected.is_some(),
        )?;
        self.button(
            panel_button(l, 2),
            &format!(
                "Rotation: {}",
                selected
                    .map(|output| output.rotation.as_str())
                    .unwrap_or("normal")
            ),
            false,
            enabled,
        )?;
        self.button(pair_button(l, 3, false), "Mode -", false, enabled)?;
        self.button(pair_button(l, 3, true), "Mode +", false, enabled)?;
        self.button(pair_button(l, 4, false), "Rate -", false, enabled)?;
        self.button(pair_button(l, 4, true), "Rate +", false, enabled)?;
        self.label(
            l.panel.x + 10,
            l.panel.y + 402,
            &selected
                .map(|output| format!("Position: {}, {}", output.x, output.y))
                .unwrap_or_else(|| "Position: —".into()),
            self.palette.muted,
            11.0,
        )?;
        self.label(
            l.panel.x + 10,
            l.panel.y + 417,
            &selected
                .map(|output| {
                    format!(
                        "Mode: {}  {}",
                        output.mode,
                        output
                            .rate
                            .map(|rate| format!("{rate:.2} Hz"))
                            .unwrap_or_else(|| "auto rate".into())
                    )
                })
                .unwrap_or_else(|| "Mode: —".into()),
            self.palette.muted,
            11.0,
        )?;
        self.button(pair_button(l, 5, false), "Identify", false, enabled)?;
        self.button(pair_button(l, 5, true), "Mirror", false, enabled)?;
        let buttons = apply_buttons(l);
        self.button(buttons[0], "Preview", !state.preview_active(), enabled)?;
        self.button(
            buttons[1],
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
            state.preview_active(),
        )?;
        self.button(buttons[2], "Revert", false, state.preview_active())?;
        Ok(())
    }

    fn paint_profiles(&self, state: &State, l: Layout) -> Result<(), String> {
        self.fill(self.palette.surface, l.profiles)?;
        self.label(
            l.profiles.x + 8,
            l.profiles.y + 4,
            "Profiles",
            self.palette.muted,
            12.0,
        )?;
        if state.status.profiles.is_empty() {
            self.label(
                l.profiles.x + 8,
                l.profiles.y + 34,
                "No saved profiles yet. Move the screens, then use Save as….",
                self.palette.muted,
                11.0,
            )?;
        }
        for (index, profile) in state.status.profiles.iter().enumerate().take(8) {
            let selected = state.selected_profile == Some(index);
            self.button(
                profile_rect(l, index),
                &format!(
                    "{} ({})",
                    truncate(&profile.name, 22),
                    profile.outputs.len()
                ),
                selected,
                true,
            )?;
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
            self.button(action_rect(l, index), text, active && enabled, enabled)?;
        }
        Ok(())
    }

    fn paint_name(&self, state: &State) -> Result<(), String> {
        let rect = name_rect(self.w, self.h);
        self.fill(self.palette.background, rect)?;
        self.fill(
            self.palette.accent,
            Rectangle {
                width: rect.width,
                height: 2,
                ..rect
            },
        )?;
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
        self.button(
            Rectangle {
                x: rect.x + 16,
                y: rect.y + 46,
                width: rect.width.saturating_sub(32),
                height: 30,
            },
            &format!("{}_", state.name_text),
            true,
            true,
        )?;
        self.label(
            rect.x + 16,
            rect.y + 82,
            "Enter confirms · Esc cancels · spaces and Swedish letters are allowed",
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
        self.fill(
            self.palette.accent,
            Rectangle {
                width: rect.width,
                height: 2,
                ..rect
            },
        )?;
        self.label(
            rect.x + 20,
            rect.y + 16,
            "NIRU Display",
            self.palette.foreground,
            20.0,
        )?;
        let lines = [
            "Mouse        drag a screen; edges snap to the next screen",
            "Tab          move between every control",
            "Arrows       move the selected screen (Shift for 10 px)",
            "Space/Enter  press the focused control",
            "I            identify each screen with a number",
            "R            reload the actual display state",
            "F1           open or close this help",
            "Esc/Q        close; an active preview still reverts on timeout",
            "Profiles     click to load, then Preview; Save as… stores the draft",
            "Preview      changes stay for 15 s; Keep confirms, Revert restores",
        ];
        for (index, line) in lines.iter().enumerate() {
            self.label(
                rect.x + 20,
                rect.y + 58 + index as i16 * 24,
                line,
                self.palette.muted,
                13.0,
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

fn focus_rect(state: &State, l: Layout, canvas: Canvas, focus: Hit) -> Option<Rectangle> {
    match focus {
        Hit::Monitor(index) => state
            .draft
            .get(index)
            .filter(|output| output.enabled)
            .map(|output| monitor_rect(canvas, output)),
        Hit::Output(index) => Some(output_row(l, index)),
        Hit::Primary => Some(panel_button(l, 0)),
        Hit::Enabled => Some(panel_button(l, 1)),
        Hit::Rotate => Some(panel_button(l, 2)),
        Hit::ModePrev => Some(pair_button(l, 3, false)),
        Hit::ModeNext => Some(pair_button(l, 3, true)),
        Hit::RatePrev => Some(pair_button(l, 4, false)),
        Hit::RateNext => Some(pair_button(l, 4, true)),
        Hit::Identify => Some(pair_button(l, 5, false)),
        Hit::Mirror => Some(pair_button(l, 5, true)),
        Hit::Apply => Some(apply_buttons(l)[0]),
        Hit::Keep => Some(apply_buttons(l)[1]),
        Hit::Revert => Some(apply_buttons(l)[2]),
        Hit::Profile(index) => Some(profile_rect(l, index)),
        Hit::Save => Some(action_rect(l, 0)),
        Hit::Rename => Some(action_rect(l, 1)),
        Hit::Delete => Some(action_rect(l, 2)),
        Hit::Startup => Some(action_rect(l, 3)),
        Hit::Reload => Some(action_rect(l, 4)),
        Hit::NameField => None,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
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
                                    Some(NameAction::Save) => state.save(&text),
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
                    if help {
                        help = false;
                        continue;
                    }
                    if key == K_ESCAPE || key == K_Q || key == K_Q_SHIFTED {
                        break 'outer;
                    }
                    if key == K_F1 {
                        help = true;
                        continue;
                    }
                    if key == K_I {
                        if let Err(err) = app.identify(state) {
                            state.set_message(err, true);
                        }
                        state.identify_until = app.identify_until;
                        continue;
                    }
                    if key == K_R {
                        if let Err(err) = state.reload(true) {
                            state.set_message(err, true);
                        }
                        dirty = true;
                        continue;
                    }
                    if key == K_TAB {
                        let order = focus_order(state);
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
                    if matches!(key, K_LEFT | K_RIGHT | K_UP | K_DOWN) {
                        let step = if shift { 10 } else { 1 };
                        match key {
                            K_LEFT => state.move_selected(-step, 0),
                            K_RIGHT => state.move_selected(step, 0),
                            K_UP => state.move_selected(0, -step),
                            K_DOWN => state.move_selected(0, step),
                            _ => {}
                        }
                        continue;
                    }
                    if key == K_RETURN || key == K_SPACE {
                        if let Some(focus) = state.focus {
                            activate(app, state, focus)?;
                        } else if key == K_RETURN {
                            if let Err(err) = state.apply_preview() {
                                state.set_message(err, true);
                            }
                        }
                        continue;
                    }
                    // Plain arrows already moved the monitor; keep the selected
                    // screen visible when Tab focus is not involved.
                }
                Event::ButtonPress(e) => {
                    state.focus = None;
                    if e.detail == 1 {
                        let l = layout(app.w, app.h);
                        let canvas = canvas_for(l.canvas, &state.draft);
                        if let Some(Hit::Monitor(index)) =
                            hit(state, l, canvas, e.event_x, e.event_y)
                        {
                            state.selected = index;
                            if let Some(output) = state.current() {
                                app.drag = Some((index, e.event_x, e.event_y, output.x, output.y));
                            }
                            continue;
                        }
                        if let Some(action) = hit(state, l, canvas, e.event_x, e.event_y) {
                            activate(app, state, action)?;
                            continue;
                        }
                    }
                }
                Event::ButtonRelease(_) => {
                    app.drag = None;
                }
                Event::MotionNotify(e) => {
                    if let Some((index, start_x, start_y, start_output_x, start_output_y)) =
                        app.drag
                    {
                        let l = layout(app.w, app.h);
                        let canvas = canvas_for(l.canvas, &state.draft);
                        let dx = ((e.event_x - start_x) as f32 / canvas.scale).round() as i32;
                        let dy = ((e.event_y - start_y) as f32 / canvas.scale).round() as i32;
                        if let Some(output) = state.draft.get_mut(index) {
                            output.x = start_output_x + dx;
                            output.y = start_output_y + dy;
                            snap(&mut state.draft, index);
                            state.dirty = true;
                        }
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
                        break 'outer;
                    }
                }
                _ => {}
            }
        }
        if let Some(until) = state.identify_until {
            if Instant::now() >= until {
                let _ = app.clear_identify();
                state.identify_until = None;
            }
        }
        let interval = if state.preview_active() {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(2)
        };
        if last_refresh.elapsed() >= interval && state.name_mode.is_none() {
            last_refresh = Instant::now();
            if let Err(err) = state.reload(false) {
                state.set_message(err, true);
            }
            dirty = true;
        }
        if dirty {
            app.palette = theme::load();
            app.paint(state, layout(app.w, app.h), help)?;
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

fn focus_order(state: &State) -> Vec<Hit> {
    let mut out = Vec::new();
    for index in 0..state.draft.len().min(5) {
        out.push(Hit::Output(index));
    }
    out.extend([
        Hit::Primary,
        Hit::Enabled,
        Hit::Rotate,
        Hit::ModePrev,
        Hit::ModeNext,
        Hit::RatePrev,
        Hit::RateNext,
        Hit::Identify,
        Hit::Mirror,
        Hit::Apply,
        Hit::Keep,
        Hit::Revert,
    ]);
    for index in 0..state.status.profiles.len().min(8) {
        out.push(Hit::Profile(index));
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

fn activate(app: &mut App, state: &mut State, action: Hit) -> Result<(), String> {
    match action {
        Hit::Monitor(index) | Hit::Output(index) => {
            if index < state.draft.len() {
                state.selected = index;
            }
        }
        Hit::Profile(index) => {
            if let Err(err) = state.load_profile(index) {
                state.set_message(err, true);
            }
        }
        Hit::Enabled => state.toggle_enabled(),
        Hit::Primary => state.set_primary(),
        Hit::Rotate => state.rotate(),
        Hit::ModePrev => state.cycle_mode(-1),
        Hit::ModeNext => state.cycle_mode(1),
        Hit::RatePrev => state.cycle_rate(-1),
        Hit::RateNext => state.cycle_rate(1),
        Hit::Identify => {
            app.identify(state)?;
            state.identify_until = app.identify_until;
        }
        Hit::Mirror => state.mirror_screens(),
        Hit::Apply => {
            if let Err(err) = state.apply_preview() {
                state.set_message(err, true);
            }
        }
        Hit::Keep => {
            if let Err(err) = state.keep() {
                state.set_message(err, true);
            }
        }
        Hit::Revert => {
            if let Err(err) = state.revert() {
                state.set_message(err, true);
            }
        }
        Hit::Save => {
            state.name_mode = Some(NameAction::Save);
            state.name_text = state
                .selected_profile
                .and_then(|index| state.status.profiles.get(index))
                .map(|profile| profile.name.clone())
                .unwrap_or_default();
            state.focus = Some(Hit::NameField);
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
        }
        Hit::Delete => {
            if let Err(err) = state.delete() {
                state.set_message(err, true);
            }
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
        }
        Hit::Reload => {
            if let Err(err) = state.reload(true) {
                state.set_message(err, true);
            }
        }
        Hit::NameField => {}
    }
    Ok(())
}

fn snap(draft: &mut [DraftOutput], index: usize) {
    let Some(output) = draft.get(index).cloned() else {
        return;
    };
    let (w, h) = (output.width(), output.height());
    let threshold = 18;
    let mut x = output.x;
    let mut y = output.y;
    for (other_index, other) in draft.iter().enumerate() {
        if other_index == index || !other.enabled {
            continue;
        }
        let (ow, oh) = (other.width(), other.height());
        for candidate in [other.x, other.x + ow, other.x - w, other.x + ow - w] {
            if (x - candidate).abs() < threshold {
                x = candidate;
            }
        }
        for candidate in [other.y, other.y + oh, other.y - h, other.y + oh - h] {
            if (y - candidate).abs() < threshold {
                y = candidate;
            }
        }
    }
    if x.abs() < threshold {
        x = 0;
    }
    if y.abs() < threshold {
        y = 0;
    }
    if let Some(output) = draft.get_mut(index) {
        output.x = x;
        output.y = y;
    }
}
