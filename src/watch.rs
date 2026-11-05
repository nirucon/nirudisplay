//! Knowing when the display state may have changed, without asking.
//!
//! The window used to run `niructl display status --json` every 2 s (every
//! second during a preview), and each run made `xrandr` reprobe every output
//! and read its EDID over DDC. Now it listens: RandR events on the root window
//! say when the hardware or the layout changed, inotify says when `niructl`
//! wrote the profile file or the preview transaction, and a stamp guard keeps
//! the window's own status read from triggering another one. A burst of
//! changes is debounced into one refresh. The debounce, the wake pipe and the
//! watcher are shared with NIRU Theme Selector and NIRU Wallpaper 0.2.0.
//! The worker thread wakes the event loop through a pipe, so the loop can
//! block in `poll` with no timeout when nothing is pending.

use std::{
    ffi::CString,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long the window waits after the last change before it refreshes.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// A steady stream of changes still refreshes at least this often.
pub const DEBOUNCE_MAX: Duration = Duration::from_millis(1000);

/// A trailing debounce: the refresh runs `DEBOUNCE` after the last change,
/// and never later than `DEBOUNCE_MAX` after the first one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Debounce {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Debounce {
    /// Record one change.
    pub fn note(&mut self, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    fn due_at(&self) -> Option<Instant> {
        let first = self.first?;
        let last = self.last?;
        Some((last + DEBOUNCE).min(first + DEBOUNCE_MAX))
    }

    /// How long the event loop may sleep, or `None` for "until an event".
    pub fn timeout(&self, now: Instant) -> Option<Duration> {
        self.due_at().map(|at| at.saturating_duration_since(now))
    }

    /// True once, when the refresh is due; the debounce is then reset.
    pub fn take(&mut self, now: Instant) -> bool {
        match self.due_at() {
            Some(at) if now >= at => {
                *self = Self::default();
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn pending(&self) -> bool {
        self.first.is_some()
    }
}

/// A pipe a worker thread writes one byte to, to wake the event loop.
pub struct Wake {
    read: libc::c_int,
    write: libc::c_int,
}

impl Wake {
    pub fn new() -> Result<Self, String> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors pipe2 returns.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if rc != 0 {
            return Err(format!(
                "cannot create a wake-up pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            read: fds[0],
            write: fds[1],
        })
    }

    pub fn fd(&self) -> libc::c_int {
        self.read
    }

    /// Wake the event loop. A full pipe already means "wake up".
    pub fn notify(&self) {
        let byte = 1u8;
        // SAFETY: one byte from a live buffer to a descriptor this owns.
        let _ = unsafe { libc::write(self.write, (&byte as *const u8).cast(), 1) };
    }

    /// Empty the pipe after waking.
    pub fn drain(&self) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: reads into a live buffer of the given length.
            let n = unsafe { libc::read(self.read, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return;
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: both descriptors are owned here and closed once.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

/// A fingerprint of the screen's RandR state: the change stamps
/// (`timestamp`, the last time a layout was set, and `config_timestamp`,
/// the last time the hardware changed), the screen size, every CRTC's
/// geometry, mode, rotation and outputs, and every output's connection.
/// It is read with `GetScreenResourcesCurrent`, which does not probe.
pub type Stamps = u64;

/// Decides whether a RandR event needs a new status read.
///
/// `niructl display status` runs `xrandr --query`, which reprobes every
/// output; on some drivers that alone sends output or property events. The
/// stamps are recorded after every status read, so an event that only
/// reports the window's own reprobe finds them unchanged and is dropped,
/// and the window cannot feed itself a reload loop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StampGuard {
    seen: Option<Stamps>,
}

impl StampGuard {
    /// Remember the stamps a status read was made at.
    pub fn record(&mut self, stamps: Option<Stamps>) {
        if stamps.is_some() {
            self.seen = stamps;
        }
    }

    /// True when the stamps differ from the last status read, or when they
    /// cannot be read at all (then every event counts).
    pub fn changed(&self, stamps: Option<Stamps>) -> bool {
        match (stamps, self.seen) {
            (Some(now), Some(seen)) => now != seen,
            _ => true,
        }
    }
}

/// The identity of a file's content as far as a reload cares: inode, size,
/// modification time. `None` when the file does not exist.
pub type FileStamp = Option<(u64, u64, i64, i64)>;

pub fn file_stamp(path: &Path) -> FileStamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec()))
}

/// What a change on disk can mean for the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// The theme or the appearance overrides: repaint with a new palette.
    pub theme: bool,
    /// `displays.toml` or the preview transaction: read the status again.
    pub display: bool,
}

impl Changes {
    #[cfg(test)]
    pub fn any(&self) -> bool {
        self.theme || self.display
    }

    fn merge(&mut self, other: Changes) {
        self.theme |= other.theme;
        self.display |= other.display;
    }
}

/// What one watched directory is for, which decides which names matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `~/.config/niruwm`: the theme selection, the appearance overrides and
    /// the display profiles.
    Config,
    /// `~/.config/niruwm/themes`: any visible user theme file.
    Themes,
    /// `~/.local/state/niruwm`: the preview transaction.
    State,
    /// A missing directory's nearest parent: any entry, so its creation is
    /// noticed and the watch moves to it.
    Parent,
}

/// What an event on `name` in a directory of this kind changes.
pub fn relevant(kind: Kind, name: &str) -> Changes {
    match kind {
        Kind::Config => Changes {
            theme: matches!(name, "config.toml" | "appearance.toml" | "themes" | ""),
            display: matches!(name, "displays.toml" | ""),
        },
        // Hidden names are temporary files; their final rename is what counts.
        Kind::Themes => Changes {
            theme: name.is_empty() || !name.starts_with('.'),
            display: false,
        },
        Kind::State => Changes {
            theme: false,
            display: matches!(name, "display-preview.toml" | ""),
        },
        Kind::Parent => Changes {
            theme: true,
            display: true,
        },
    }
}

/// The NIRUWM configuration directory, as `niructl` and the theme use it.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("niruwm")
}

/// The NIRUWM state directory that holds `display-preview.toml`.
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("niruwm")
}

/// The two files whose content decides the profile list and the preview.
pub fn display_files() -> [PathBuf; 2] {
    [
        config_dir().join("displays.toml"),
        state_dir().join("display-preview.toml"),
    ]
}

pub fn watched_dirs() -> Vec<(PathBuf, Kind)> {
    let config = config_dir();
    vec![
        (config.clone(), Kind::Config),
        (config.join("themes"), Kind::Themes),
        (state_dir(), Kind::State),
    ]
}

const MASK: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;

/// An inotify instance watching a set of directories.
pub struct Watcher {
    fd: libc::c_int,
    /// Watch descriptor, the directory as asked for, and its kind.
    watches: Vec<(libc::c_int, PathBuf, Kind)>,
    /// The set last asked for, so an unchanged set is not re-added.
    wanted: Vec<(PathBuf, Kind)>,
}

impl Watcher {
    pub fn new() -> Result<Self, String> {
        // SAFETY: plain syscall with flags.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(format!(
                "cannot watch the display files: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self {
            fd,
            watches: Vec::new(),
            wanted: Vec::new(),
        })
    }

    pub fn fd(&self) -> libc::c_int {
        self.fd
    }

    fn add(&mut self, path: &Path, kind: Kind) -> bool {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a valid NUL-terminated path and a live inotify descriptor.
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
        if wd < 0 {
            return false;
        }
        if !self.watches.iter().any(|(w, _, _)| *w == wd) {
            self.watches.push((wd, path.to_path_buf(), kind));
        }
        true
    }

    /// Watch exactly these directories. A directory that does not exist yet
    /// is watched through its nearest existing parent.
    pub fn set(&mut self, dirs: Vec<(PathBuf, Kind)>) {
        let all_present = self.watches.iter().all(|(_, _, k)| *k != Kind::Parent);
        if dirs == self.wanted && all_present {
            return;
        }
        for (wd, _, _) in self.watches.drain(..) {
            // SAFETY: removing a watch this instance added.
            unsafe { libc::inotify_rm_watch(self.fd, wd) };
        }
        for (path, kind) in &dirs {
            if self.add(path, *kind) {
                continue;
            }
            let mut parent = path.parent();
            while let Some(dir) = parent {
                if dir.is_dir() {
                    self.add(dir, Kind::Parent);
                    break;
                }
                parent = dir.parent();
            }
        }
        self.wanted = dirs;
    }

    /// Read every queued event and say what they can change.
    pub fn drain(&mut self) -> Changes {
        let mut seen = Changes::default();
        let mut buf = [0u8; 4096];
        loop {
            // SAFETY: reads into a live, suitably sized buffer.
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return seen;
            }
            let n = n as usize;
            let mut at = 0;
            let header = std::mem::size_of::<libc::inotify_event>();
            while at + header <= n {
                // SAFETY: the kernel wrote a whole event header at `at`;
                // read_unaligned copes with the byte buffer's alignment.
                let event: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at).cast()) };
                let len = event.len as usize;
                let raw = &buf[(at + header).min(n)..(at + header + len).min(n)];
                let name = raw.split(|b| *b == 0).next().unwrap_or_default();
                let name = String::from_utf8_lossy(name);
                let kind = self
                    .watches
                    .iter()
                    .find(|(wd, _, _)| *wd == event.wd)
                    .map(|(_, _, kind)| *kind);
                if event.mask & libc::IN_Q_OVERFLOW != 0 {
                    seen.merge(relevant(Kind::Parent, ""));
                }
                if let Some(kind) = kind {
                    if event.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED)
                        != 0
                    {
                        // The directory itself went away: watch its parent
                        // on the next `set`.
                        self.wanted.clear();
                        seen.merge(relevant(Kind::Parent, ""));
                    } else {
                        seen.merge(relevant(kind, &name));
                    }
                }
                at += header + len;
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned here and closed once.
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_changes_is_one_refresh_300_ms_after_the_last() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        assert_eq!(d.timeout(t0), None, "nothing pending: block forever");
        assert!(!d.take(t0));
        d.note(t0);
        d.note(t0 + Duration::from_millis(100));
        d.note(t0 + Duration::from_millis(200));
        assert!(!d.take(t0 + Duration::from_millis(400)), "too early");
        assert_eq!(
            d.timeout(t0 + Duration::from_millis(400)),
            Some(Duration::from_millis(100))
        );
        assert!(d.take(t0 + Duration::from_millis(500)));
        assert!(!d.take(t0 + Duration::from_millis(600)), "only once");
        assert!(!d.pending());
    }

    #[test]
    fn a_steady_stream_of_events_still_refreshes_within_a_second() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        let mut refreshed_at = None;
        for step in 0..30u64 {
            let now = t0 + Duration::from_millis(step * 100);
            if d.take(now) {
                refreshed_at = Some(step * 100);
                break;
            }
            d.note(now);
        }
        assert_eq!(refreshed_at, Some(1000));
    }

    #[test]
    fn the_windows_own_reprobe_cannot_start_a_reload_loop() {
        let mut guard = StampGuard::default();
        assert!(guard.changed(Some(57)), "nothing recorded yet");
        // The window reads the status; niructl's xrandr --query reprobes and
        // the driver sends output/property events, but the fingerprint
        // recorded after the read is the one the events report.
        guard.record(Some(57));
        assert!(!guard.changed(Some(57)), "own reprobe is no change");
        // A cable, `xrandr --output … --mode …` or a new screen size
        // changes the fingerprint.
        assert!(guard.changed(Some(58)), "a new layout");
        // Without RandR every event counts, and an unreadable read keeps
        // the last good fingerprint.
        assert!(guard.changed(None));
        guard.record(None);
        assert!(!guard.changed(Some(57)));
    }

    #[test]
    fn only_the_files_that_change_the_window_count() {
        assert!(relevant(Kind::Config, "displays.toml").display);
        assert!(!relevant(Kind::Config, "displays.toml").theme);
        assert!(!relevant(Kind::Config, "displays.new").any(), "temp file");
        assert!(relevant(Kind::Config, "config.toml").theme);
        assert!(relevant(Kind::Config, "appearance.toml").theme);
        assert!(!relevant(Kind::Config, "config.toml.bak").any());
        assert!(relevant(Kind::State, "display-preview.toml").display);
        assert!(!relevant(Kind::State, "display-preview.new").any());
        assert!(!relevant(Kind::State, "session.toml").any());
        assert!(relevant(Kind::Themes, "mine.toml").theme);
        assert!(!relevant(Kind::Themes, ".mine.toml.swp").any());
    }

    #[test]
    fn a_profile_save_and_a_missing_state_directory_are_noticed() {
        let root = std::env::temp_dir().join(format!("nirudisplay-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        let mut w = Watcher::new().unwrap();
        let dirs = vec![
            (root.join("config"), Kind::Config),
            (root.join("state/niruwm"), Kind::State),
        ];
        w.set(dirs.clone());
        assert!(!w.drain().any(), "nothing happened yet");
        // niructl writes displays.new and renames it into place.
        std::fs::write(root.join("config/displays.new"), b"x").unwrap();
        assert!(!w.drain().any(), "the temporary file is not a change");
        std::fs::rename(
            root.join("config/displays.new"),
            root.join("config/displays.toml"),
        )
        .unwrap();
        assert!(w.drain().display, "the rename into place is");
        let before = file_stamp(&root.join("config/displays.toml"));
        assert!(before.is_some());
        assert_eq!(before, file_stamp(&root.join("config/displays.toml")));
        assert_eq!(file_stamp(&root.join("config/none.toml")), None);
        // The missing state directory is watched through its parent.
        std::fs::create_dir_all(root.join("state/niruwm")).unwrap();
        assert!(w.drain().any());
        w.set(dirs);
        std::fs::write(root.join("state/niruwm/display-preview.toml"), b"x").unwrap();
        assert!(w.drain().display, "the directory is watched once it exists");
        std::fs::remove_dir_all(root).unwrap();
    }
}
