//! The display state and actions owned by niructl.
//!
//! This program has no display parser, profile store, apply policy or rollback
//! timer of its own. It reads the versioned JSON report and asks niructl to
//! preview, keep, revert and save, so the GUI and the terminal cannot disagree.

use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CurrentOutput {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub position: String,
    #[serde(default)]
    pub rotation: String,
    #[serde(default)]
    pub primary: bool,
}

impl CurrentOutput {
    pub fn position_pair(&self) -> Option<(i32, i32)> {
        position_pair(&self.position)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DisplayMode {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub rates: Vec<f64>,
    #[serde(default)]
    pub preferred: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub geometry: Option<Geometry>,
    #[serde(default)]
    pub current: Option<CurrentOutput>,
    #[serde(default)]
    pub modes: Vec<DisplayMode>,
}

impl OutputState {
    pub fn usable(&self) -> bool {
        self.connected && !self.modes.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileOutput {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub position: String,
    #[serde(default)]
    pub rotation: String,
    #[serde(default)]
    pub primary: bool,
}

impl ProfileOutput {
    pub fn position_pair(&self) -> Option<(i32, i32)> {
        position_pair(&self.position)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub outputs: Vec<ProfileOutput>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub seconds_left: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub outputs: Vec<OutputState>,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub startup_profile: Option<String>,
    #[serde(default)]
    pub preview: Option<Preview>,
}

impl Status {
    pub fn output(&self, name: &str) -> Option<&OutputState> {
        self.outputs.iter().find(|output| output.name == name)
    }
}

pub fn position_pair(position: &str) -> Option<(i32, i32)> {
    let position = position.trim_start_matches('+');
    let (x, y) = position.split_once('x')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

pub fn position_string(x: i32, y: i32) -> String {
    format!("{x}x{y}")
}

pub fn find_niructl() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("NIRUDISPLAY_NIRUCTL").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".local/share/nirux/current/bin/niructl"));
        candidates.push(home.join(".local/bin/niructl"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("niructl"));
        }
    }
    candidates.into_iter().find(|path| path.is_file())
}

pub fn niructl() -> Result<PathBuf, String> {
    find_niructl().ok_or_else(|| {
        "the niructl command was not found; install NIRUX or run this from a NIRUX session"
            .to_string()
    })
}

fn run(args: &[&str]) -> Result<Output, String> {
    let bin = niructl()?;
    let out = Command::new(&bin)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("no explanation");
        return Err(why.to_string());
    }
    Ok(out)
}

fn run_stdin(args: &[&str], input: &str) -> Result<Output, String> {
    let bin = niructl()?;
    let mut child = Command::new(&bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    child
        .stdin
        .as_mut()
        .ok_or("cannot open niructl stdin")?
        .write_all(input.as_bytes())
        .map_err(|e| format!("cannot send the display profile: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("cannot read niructl output: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("no explanation");
        return Err(why.to_string());
    }
    Ok(out)
}

pub fn read_status() -> Result<Status, String> {
    let out = run(&["display", "status", "--json"])?;
    serde_json::from_slice(&out.stdout).map_err(|e| format!("invalid display status JSON: {e}"))
}

pub fn preview(profile: &Profile, seconds: u64) -> Result<(), String> {
    let text = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    run_stdin(&["display", "preview-json", &seconds.to_string()], &text).map(|_| ())
}

pub fn keep() -> Result<(), String> {
    run(&["display", "keep"]).map(|_| ())
}

pub fn revert() -> Result<(), String> {
    run(&["display", "revert"]).map(|_| ())
}

pub fn save_profile(profile: &Profile, name: &str) -> Result<(), String> {
    let text = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    run_stdin(&["display", "save-json", name], &text).map(|_| ())
}

pub fn delete_profile(name: &str) -> Result<(), String> {
    run(&["display", "delete", name]).map(|_| ())
}

pub fn rename_profile(old: &str, new: &str) -> Result<(), String> {
    run(&["display", "rename", old, new]).map(|_| ())
}

pub fn set_startup(name: Option<&str>) -> Result<(), String> {
    match name {
        Some(name) => run(&["display", "startup", "set", name]).map(|_| ()),
        None => run(&["display", "startup", "off"]).map(|_| ()),
    }
}

pub fn status_text() -> Result<String, String> {
    let status = read_status()?;
    let mut text = String::new();
    for output in &status.outputs {
        if !output.connected {
            continue;
        }
        let current = output.current.as_ref();
        text.push_str(&format!(
            "{}\tconnected={}\tactive={}\tprimary={}\t{}\n",
            output.name,
            output.connected,
            output.active,
            current.map(|current| current.primary).unwrap_or(false),
            current
                .map(|current| format!(
                    "{}@{}{}",
                    current.mode,
                    current
                        .rate
                        .map(|rate| format!("{rate:.2}Hz"))
                        .unwrap_or_else(|| "auto".into()),
                    if current.rotation.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", current.rotation)
                    }
                ))
                .unwrap_or_else(|| "off".into())
        ));
    }
    for profile in &status.profiles {
        text.push_str(&format!(
            "profile={}\toutputs={}{}\n",
            profile.name,
            profile.outputs.len(),
            if status.startup_profile.as_deref() == Some(profile.name.as_str()) {
                "\tstartup"
            } else {
                ""
            }
        ));
    }
    if let Some(preview) = &status.preview {
        text.push_str(&format!(
            "preview={}\tseconds_left={}\n",
            preview.name, preview.seconds_left
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "schema": 1,
      "outputs": [{
        "name": "eDP-1", "connected": true, "active": true, "primary": true,
        "geometry": {"x":0,"y":0,"width":1920,"height":1080},
        "current": {"mode":"1920x1080","rate":60.0,"position":"0x0","rotation":"normal","primary":true},
        "modes": [{"mode":"1920x1080","rates":[60.0,59.94],"preferred":true}]
      }],
      "profiles": [{"name":"Laptop","outputs":[{"name":"eDP-1","mode":"1920x1080","rate":60.0,"position":"0x0","rotation":"normal","primary":true}]}],
      "startup_profile": "Laptop",
      "preview": null
    }"#;

    #[test]
    fn parses_the_versioned_status() {
        let status: Status = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(status.schema, 1);
        assert_eq!(status.outputs[0].name, "eDP-1");
        assert_eq!(status.outputs[0].geometry.as_ref().unwrap().width, 1920);
        assert_eq!(status.outputs[0].current.as_ref().unwrap().rate, Some(60.0));
        assert_eq!(status.profiles[0].name, "Laptop");
        assert_eq!(status.startup_profile.as_deref(), Some("Laptop"));
    }

    #[test]
    fn positions_round_trip() {
        assert_eq!(position_pair("2560x0"), Some((2560, 0)));
        assert_eq!(position_pair("+0x1440"), Some((0, 1440)));
        assert_eq!(position_pair("bad"), None);
        assert_eq!(position_string(-1, 20), "-1x20");
    }

    #[test]
    fn unknown_fields_and_missing_rates_remain_compatible() {
        let text = r#"{"schema":1,"outputs":[],"profiles":[{"name":"Old","outputs":[{"name":"eDP-1","mode":"1920x1080","position":"0x0","rotation":"normal","primary":true}]}],"extra":true}"#;
        let status: Status = serde_json::from_str(text).unwrap();
        assert_eq!(status.profiles[0].outputs[0].rate, None);
    }
}
