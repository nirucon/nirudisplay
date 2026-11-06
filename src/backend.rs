//! The display state and actions owned by niructl.
//!
//! This program has no display parser, profile store, apply policy or rollback
//! timer of its own. It reads the versioned JSON report and asks niructl to
//! preview, keep, revert and save, so the GUI and the terminal cannot disagree.

use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::io::AsRawFd,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
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
pub struct Identity {
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub product_code: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputState {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub identity: Option<Identity>,
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
    /// The built-in panel of a laptop (eDP, LVDS, DSI).
    #[serde(default)]
    pub internal: bool,
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
    /// niructl's description of the screen the entry wants ("HPN HP E27q G5
    /// (serial …2MRJ)", or the connector for profiles without identities).
    /// Display only; never sent back.
    #[serde(default, skip_serializing)]
    pub label: Option<String>,
}

impl ProfileOutput {
    pub fn position_pair(&self) -> Option<(i32, i32)> {
        position_pair(&self.position)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub outputs: Vec<ProfileOutput>,
    /// Takes part in automatic switching (niructl `auto`, default true).
    #[serde(default = "yes", skip_serializing)]
    pub auto: bool,
    /// How the profile fits the screens connected now: `exact`, `partial` or
    /// `missing`. None from an older niructl.
    #[serde(default, skip_serializing)]
    pub fit: Option<String>,
    /// Why it does not fit, e.g. "needs HPN HP E27q G5 (serial …2MRJ)".
    #[serde(default, skip_serializing)]
    pub reason: Option<String>,
    /// Every screen it names is connected, so it can be loaded. None from
    /// an older niructl.
    #[serde(default, skip_serializing)]
    pub loadable: Option<bool>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::new(),
            outputs: Vec::new(),
            auto: true,
            fit: None,
            reason: None,
            loadable: None,
        }
    }
}

fn yes() -> bool {
    true
}

/// What the automation did last.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutoEvent {
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub ok: bool,
}

/// Automatic switching, as niructl reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Automatic {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub lid: Option<String>,
    #[serde(default)]
    pub last: Option<AutoEvent>,
    #[serde(default)]
    pub retry_in: Option<u64>,
    #[serde(default)]
    pub ambiguous: Vec<String>,
    /// Set while the screens show a layout the automation made (remembered
    /// or extended) rather than a saved profile.
    #[serde(default)]
    pub automatic_layout: Option<String>,
    #[serde(default)]
    pub current_profile: Option<String>,
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
    /// None from an older niructl, which has no automatic switching.
    #[serde(default)]
    pub automatic: Option<Automatic>,
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
    // The development override is strict: a test that names a fixture must
    // never fall through to the real niructl and the owner's profiles.
    if let Some(path) = std::env::var_os("NIRUDISPLAY_NIRUCTL").map(PathBuf::from) {
        return path.is_file().then_some(path);
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

/// Status, save, rename, delete and startup answer quickly; a stuck
/// `niructl` is stopped after this long.
pub const QUICK_DEADLINE: Duration = Duration::from_secs(5);
/// Preview, keep and revert apply a layout, and the outputs can take
/// seconds to resync.
pub const APPLY_DEADLINE: Duration = Duration::from_secs(20);

fn run(args: &[&str], deadline: Duration) -> Result<Output, String> {
    run_with_input(args, None, deadline)
}

fn run_stdin(args: &[&str], input: &str, deadline: Duration) -> Result<Output, String> {
    run_with_input(args, Some(input), deadline)
}

fn run_with_input(
    args: &[&str],
    input: Option<&str>,
    deadline: Duration,
) -> Result<Output, String> {
    let bin = niructl()?;
    let out = output_within(Command::new(&bin).args(args), input, deadline).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            format!(
                "niructl gave no answer within {} s and was stopped",
                deadline.as_secs()
            )
        } else {
            format!("cannot run {}: {error}", bin.display())
        }
    })?;
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

/// Run `program` with `input` on its stdin and collect its output, but never
/// for longer than `deadline`. The pipes are read without blocking, so a
/// program that stops answering (or leaves a child holding its stdout) cannot
/// hang the caller; it is killed and reaped when the time is up. The same
/// pattern as NIRUBAR's `probe::run` and NIRU Theme Selector's
/// `output_within`.
pub fn output_within(
    program: &mut Command,
    input: Option<&str>,
    deadline: Duration,
) -> std::io::Result<Output> {
    let started = Instant::now();
    let mut child = program
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        // A display profile is a few hundred bytes, far below a pipe's
        // buffer, so this write cannot block on a child that does not read.
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(error) = stdin.write_all(input.as_bytes()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
    let mut pipes = [
        child.stdout.take().map(Pipe::new),
        child.stderr.take().map(Pipe::new),
    ];
    let mut expired = false;
    while pipes.iter().any(|p| p.as_ref().is_some_and(|p| p.open)) {
        let Some(left) = deadline.checked_sub(started.elapsed()) else {
            expired = true;
            break;
        };
        let mut fds: Vec<libc::pollfd> = pipes
            .iter()
            .flatten()
            .filter(|p| p.open)
            .map(|p| libc::pollfd {
                fd: p.fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: a live array of pollfd structures and its length.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        for pipe in pipes.iter_mut().flatten() {
            pipe.read_available();
        }
    }
    // The pipes are closed: wait for the exit, still within the deadline.
    let status = loop {
        if expired {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("no answer within {} s", deadline.as_secs()),
            ));
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= deadline {
            expired = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let [stdout, stderr] = pipes.map(|p| p.map(|p| p.data).unwrap_or_default());
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// One output pipe of a child, read without blocking.
struct Pipe {
    reader: Box<dyn Read>,
    fd: libc::c_int,
    data: Vec<u8>,
    open: bool,
}

impl Pipe {
    fn new(reader: impl Read + AsRawFd + 'static) -> Self {
        let fd = reader.as_raw_fd();
        // SAFETY: changing the flags of a descriptor this process owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Self {
            reader: Box::new(reader),
            fd,
            data: Vec::new(),
            open: true,
        }
    }

    fn read_available(&mut self) {
        let mut buf = [0u8; 8192];
        while self.open {
            match self.reader.read(&mut buf) {
                Ok(0) => self.open = false,
                Ok(n) => self.data.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => self.open = false,
            }
        }
    }
}

pub fn read_status() -> Result<Status, String> {
    let out = run(&["display", "status", "--json"], QUICK_DEADLINE)?;
    serde_json::from_slice(&out.stdout).map_err(|e| format!("invalid display status JSON: {e}"))
}

pub fn preview(profile: &Profile, seconds: u64) -> Result<(), String> {
    let text = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    let output = run_stdin(
        &["display", "preview-json", &seconds.to_string()],
        &text,
        APPLY_DEADLINE,
    )?;
    let token = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("preview-token: ").map(str::to_owned))
        .ok_or("backend did not return a display transaction token")?;
    *PREVIEW_TOKEN
        .lock()
        .map_err(|_| "display transaction unavailable")? = Some(token);
    Ok(())
}

static PREVIEW_TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Adopt the token of a preview that was started by another window or the CLI.
pub fn adopt_preview(preview: Option<&Preview>) {
    if let Ok(mut token) = PREVIEW_TOKEN.lock() {
        *token = preview.map(|preview| preview.token.clone());
    }
}

fn finish_preview(action: &str) -> Result<(), String> {
    let mut token = PREVIEW_TOKEN
        .lock()
        .map_err(|_| "display transaction unavailable")?;
    let identity = token
        .as_deref()
        .ok_or("this window has no display preview to confirm")?;
    run(&["display", action, identity], APPLY_DEADLINE)?;
    *token = None;
    Ok(())
}

pub fn keep() -> Result<(), String> {
    finish_preview("keep")
}

pub fn revert() -> Result<(), String> {
    finish_preview("revert")
}

pub fn save_profile(profile: &Profile, name: &str) -> Result<(), String> {
    let text = serde_json::to_string(profile).map_err(|e| e.to_string())?;
    run_stdin(&["display", "save-json", name], &text, QUICK_DEADLINE).map(|_| ())
}

pub fn delete_profile(name: &str) -> Result<(), String> {
    run(&["display", "delete", name], QUICK_DEADLINE).map(|_| ())
}

pub fn rename_profile(old: &str, new: &str) -> Result<(), String> {
    run(&["display", "rename", old, new], QUICK_DEADLINE).map(|_| ())
}

/// Turning it on acts on the connected screens at once, so it may apply a
/// layout: it gets the apply deadline.
pub fn set_automatic(enabled: bool) -> Result<(), String> {
    run(
        &["display", "automatic", if enabled { "on" } else { "off" }],
        APPLY_DEADLINE,
    )
    .map(|_| ())
}

pub fn set_startup(name: Option<&str>) -> Result<(), String> {
    match name {
        Some(name) => run(&["display", "startup", "set", name], QUICK_DEADLINE).map(|_| ()),
        None => run(&["display", "startup", "off"], QUICK_DEADLINE).map(|_| ()),
    }
}

/// One backend action. Every action runs on the worker thread, one at a
/// time, and is followed by a status read, so the window never blocks on
/// `niructl` and always shows what the action left behind.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Only read the status.
    Status,
    Preview {
        profile: Profile,
        seconds: u64,
    },
    Keep,
    Revert,
    Save {
        profile: Profile,
        name: String,
    },
    Rename {
        old: String,
        new: String,
    },
    Delete {
        name: String,
    },
    Startup {
        name: Option<String>,
    },
    Automatic {
        enabled: bool,
    },
}

impl Job {
    /// Run the action, then read the status, each within its deadline, and
    /// adopt the preview token the status reports. Called on the worker.
    pub fn perform(&self) -> (Result<(), String>, Result<Status, String>) {
        let result = match self {
            Job::Status => Ok(()),
            Job::Preview { profile, seconds } => preview(profile, *seconds),
            Job::Keep => keep(),
            Job::Revert => revert(),
            Job::Save { profile, name } => save_profile(profile, name),
            Job::Rename { old, new } => rename_profile(old, new),
            Job::Delete { name } => delete_profile(name),
            Job::Startup { name } => set_startup(name.as_deref()),
            Job::Automatic { enabled } => set_automatic(*enabled),
        };
        let status = read_status();
        if let Ok(status) = &status {
            adopt_preview(status.preview.as_ref());
        }
        (result, status)
    }
}

/// The answer to one job, with whatever the window attached to it.
pub struct Done<T> {
    pub seq: u64,
    pub after: T,
    pub result: Result<(), String>,
    pub status: Result<Status, String>,
}

/// One worker thread that runs jobs in the order they were submitted.
pub struct Worker<T> {
    jobs: mpsc::Sender<(u64, Job, T)>,
    done: mpsc::Receiver<Done<T>>,
    next: u64,
}

impl<T: Send + 'static> Worker<T> {
    /// Start the worker. `notify` is called after every finished job, from
    /// the worker thread, to wake the event loop.
    pub fn start(notify: impl Fn() + Send + 'static) -> Self {
        let (jobs, inbox) = mpsc::channel::<(u64, Job, T)>();
        let (outbox, done) = mpsc::channel();
        std::thread::spawn(move || {
            for (seq, job, after) in inbox {
                let (result, status) = job.perform();
                if outbox
                    .send(Done {
                        seq,
                        after,
                        result,
                        status,
                    })
                    .is_err()
                {
                    return;
                }
                notify();
            }
        });
        Self {
            jobs,
            done,
            next: 1,
        }
    }

    /// Queue a job; returns its sequence number.
    pub fn submit(&mut self, job: Job, after: T) -> u64 {
        let seq = self.next;
        self.next += 1;
        let _ = self.jobs.send((seq, job, after));
        seq
    }

    /// A finished job, if one is waiting.
    pub fn try_recv(&self) -> Option<Done<T>> {
        self.done.try_recv().ok()
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
        let identity = output
            .identity
            .as_ref()
            .map(|identity| {
                format!(
                    "{}{}",
                    identity.manufacturer,
                    identity
                        .model
                        .as_deref()
                        .map(|model| format!(" {model}"))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        text.push_str(&format!(
            "{}\tconnected={}\tactive={}\tprimary={}\t{}{}\n",
            output.name,
            output.connected,
            output.active,
            current.map(|current| current.primary).unwrap_or(false),
            if identity.is_empty() {
                String::new()
            } else {
                format!("{identity}\t")
            },
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
        "name": "eDP-1", "identity": {"manufacturer":"DEL","model":"Test Panel","serial":"123","product_code":4660}, "connected": true, "active": true, "primary": true,
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
        assert_eq!(
            status.outputs[0]
                .identity
                .as_ref()
                .unwrap()
                .model
                .as_deref(),
            Some("Test Panel")
        );
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
    fn a_program_that_does_not_answer_is_stopped_at_the_deadline() {
        let started = Instant::now();
        let error = output_within(
            Command::new("sh").args(["-c", "sleep 30"]),
            None,
            Duration::from_millis(300),
        )
        .expect_err("the deadline must stop it");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
        // A child that keeps stdout open after its parent exits cannot hang
        // the caller either.
        let started = Instant::now();
        let _ = output_within(
            Command::new("sh").args(["-c", "sleep 30 & echo started"]),
            None,
            Duration::from_millis(300),
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        // stdin reaches the program, and both pipes are collected.
        let out = output_within(
            Command::new("sh").args(["-c", "cat; echo err >&2"]),
            Some("profile"),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(out.stdout, b"profile");
        assert_eq!(out.stderr, b"err\n");
    }

    #[test]
    fn the_worker_runs_jobs_in_order_and_wakes_the_loop() {
        // A fixture niructl: status answers, everything else fails. The
        // override is strict, so the real niructl is never reached.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("nirudisplay-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("niructl");
        std::fs::write(
            &fixture,
            "#!/bin/sh\n[ \"$2\" = status ] && { echo '{\"schema\":1}'; exit 0; }\necho \"no $2\" >&2; exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("NIRUDISPLAY_NIRUCTL", &fixture);
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woken.clone();
        let mut worker = Worker::start(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let first = worker.submit(Job::Status, "first");
        let second = worker.submit(Job::Delete { name: "x".into() }, "second");
        let mut seen = Vec::new();
        let started = Instant::now();
        while seen.len() < 2 && started.elapsed() < Duration::from_secs(10) {
            if let Some(done) = worker.try_recv() {
                seen.push((
                    done.seq,
                    done.after,
                    done.result.is_ok(),
                    done.status.is_ok(),
                ));
            } else {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(
            seen,
            vec![
                (first, "first", true, true),
                (second, "second", false, true)
            ]
        );
        // The wake-up follows each answer.
        while woken.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            assert!(started.elapsed() < Duration::from_secs(10), "no wake-up");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_fields_and_missing_rates_remain_compatible() {
        let text = r#"{"schema":1,"outputs":[],"profiles":[{"name":"Old","outputs":[{"name":"eDP-1","mode":"1920x1080","position":"0x0","rotation":"normal","primary":true}]}],"extra":true}"#;
        let status: Status = serde_json::from_str(text).unwrap();
        assert_eq!(status.profiles[0].outputs[0].rate, None);
    }
}
