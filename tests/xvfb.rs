//! The window under a real X server, against a fixture `niructl`.
//!
//! Ignored by default because it needs `Xvfb`, `xdotool`, `xwd`, `xrandr`
//! and `setxkbmap`; CI runs it with `cargo test --locked --test xvfb --
//! --ignored`. The fixture serves two connected screens and one missing one,
//! keeps profiles as one JSON file each, runs a preview transaction with a
//! token and an expiry, and records every argument vector and every stdin it
//! is given. It never touches a real display or a real profile.

use std::{
    fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

const FIXTURE: &str = r#"#!/bin/sh
FX=${NDX_FX:?}
printf '%s\n' "$*" >> "$FX/argv.log"
now=$(date +%s)
outputs='{"name":"DP-1","identity":{"manufacturer":"DEL","model":"Left"},"connected":true,"active":true,"primary":true,"geometry":{"x":0,"y":0,"width":2560,"height":1440},"current":{"mode":"2560x1440","rate":59.95,"position":"0x0","rotation":"normal","primary":true},"modes":[{"mode":"2560x1440","rates":[59.95,143.91],"preferred":true},{"mode":"1920x1080","rates":[60.0,50.0]},{"mode":"3840x2160","rates":[30.0]}]},{"name":"DP-2","identity":{"manufacturer":"DEL","model":"Right"},"connected":true,"active":true,"primary":false,"geometry":{"x":2560,"y":0,"width":2560,"height":1440},"current":{"mode":"2560x1440","rate":59.95,"position":"2560x0","rotation":"normal","primary":false},"modes":[{"mode":"2560x1440","rates":[59.95],"preferred":true},{"mode":"1920x1080","rates":[60.0]}]},{"name":"HDMI-1","connected":false,"active":false,"modes":[]}'
status() {
  if [ -f "$FX/preview" ]; then
    read -r token expiry < "$FX/preview"
    left=$((expiry - now))
    if [ "$left" -le 0 ]; then rm -f "$FX/preview"; printf 'expired %s\n' "$token" >> "$FX/argv.log"; fi
  fi
  printf '{"schema":1,"outputs":[%s],"profiles":[' "$outputs"
  sep=
  for f in "$FX"/profiles/*.json; do
    [ -f "$f" ] || continue
    printf '%s' "$sep"; cat "$f"; sep=,
  done
  startup=null
  [ -f "$FX/startup" ] && startup="\"$(cat "$FX/startup")\""
  preview=null
  if [ -f "$FX/preview" ]; then
    read -r token expiry < "$FX/preview"
    preview="{\"name\":\"Draft\",\"token\":\"$token\",\"seconds_left\":$((expiry - now))}"
  fi
  printf '],"startup_profile":%s,"preview":%s}\n' "$startup" "$preview"
}
case "$1 $2" in
"display status") status ;;
"display preview-json")
  [ -f "$FX/slow" ] && sleep "$(cat "$FX/slow")"
  cat > "$FX/preview-stdin.json"
  token="t$now$$"
  printf '%s %s\n' "$token" $((now + ${3:-15})) > "$FX/preview"
  printf 'previewed %s\n' "$token" >> "$FX/argv.log"
  printf 'preview-token: %s\n' "$token" ;;
"display keep"|"display revert")
  read -r token expiry < "$FX/preview" 2>/dev/null || { echo "no preview" >&2; exit 1; }
  [ "$token" = "$3" ] || { echo "wrong token" >&2; exit 1; }
  rm -f "$FX/preview"; printf 'done %s %s\n' "$2" "$3" >> "$FX/argv.log" ;;
"display save-json")
  cat > "$FX/profiles/$3.json"; printf 'saved [%s]\n' "$3" >> "$FX/argv.log" ;;
"display delete") rm -f "$FX/profiles/$3.json" ;;
"display rename") mv "$FX/profiles/$3.json" "$FX/profiles/$4.json" ;;
"display startup")
  if [ "$3" = set ]; then printf '%s' "$4" > "$FX/startup"; else rm -f "$FX/startup"; fi ;;
*) echo "fixture: $*" >&2; exit 2 ;;
esac
"#;

/// Everything the test started, stopped and removed however it ends.
struct Session {
    root: PathBuf,
    xvfb: Child,
    app: Option<Child>,
    display: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(app) = self.app.as_mut() {
            let _ = app.kill();
            let _ = app.wait();
        }
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Session {
    fn log(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn run(&self, program: &str, args: &[&str]) -> String {
        let out = Command::new(program)
            .args(args)
            .env("DISPLAY", &self.display)
            .output()
            .unwrap_or_else(|e| panic!("{program}: {e}"));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn xdotool(&self, args: &[&str]) -> String {
        self.run("xdotool", args)
    }

    fn key(&self, keys: &[&str]) {
        let mut args = vec!["key", "--delay", "40"];
        args.extend_from_slice(keys);
        self.xdotool(&args);
        sleep(Duration::from_millis(250));
    }

    fn screenshot(&self, window: &str) -> Vec<u8> {
        Command::new("xwd")
            .args(["-silent", "-id", window])
            .env("DISPLAY", &self.display)
            .output()
            .expect("xwd")
            .stdout
    }

    /// Wait until the fixture log satisfies `test`, or fail after `limit`.
    fn wait_log(&self, limit: Duration, what: &str, test: impl Fn(&[String]) -> bool) {
        let start = Instant::now();
        while !test(&self.log()) {
            assert!(
                start.elapsed() < limit,
                "timed out waiting for {what}: {:?}",
                self.log()
            );
            sleep(Duration::from_millis(25));
        }
    }

    fn status_reads(&self) -> usize {
        count(&self.log(), "display status --json")
    }

    fn exited(&mut self) -> bool {
        self.app
            .as_mut()
            .is_some_and(|app| app.try_wait().ok().flatten().is_some())
    }
}

fn start() -> Session {
    let root = std::env::temp_dir().join(format!("ndx-xvfb-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for dir in [
        "profiles",
        "home/.config/niruwm",
        "home/.local/state/niruwm",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    // One profile that needs a screen this fixture does not connect.
    fs::write(
        root.join("profiles/TV.json"),
        r#"{"name":"TV","outputs":[{"name":"HDMI-1","mode":"1920x1080","rate":60.0,"position":"0x0","rotation":"normal","primary":true}]}"#,
    )
    .unwrap();
    fs::write(
        root.join("profiles/Work.json"),
        r#"{"name":"Work","outputs":[{"name":"DP-1","mode":"2560x1440","rate":59.95,"position":"0x0","rotation":"normal","primary":true}]}"#,
    )
    .unwrap();
    let niructl = root.join("niructl");
    fs::write(&niructl, FIXTURE).unwrap();
    fs::set_permissions(&niructl, fs::Permissions::from_mode(0o755)).unwrap();
    // Xvfb picks a free display and names it on the descriptor given, so
    // the test never collides with another X server on the host. -noreset
    // keeps the keymap set by setxkbmap when a short-lived client leaves.
    let mut xvfb = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-noreset",
            "-screen",
            "0",
            "1280x800x24",
            "-nolisten",
            "tcp",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Xvfb must be installed for this test");
    let mut line = String::new();
    BufReader::new(xvfb.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let display = format!(":{}", line.trim());
    Session {
        root,
        xvfb,
        app: None,
        display,
    }
}

fn open_window(s: &mut Session) -> String {
    let home = s.root.join("home");
    let app = Command::new(env!("CARGO_BIN_EXE_nirudisplay"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("DISPLAY", &s.display)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("NIRUDISPLAY_NIRUCTL", s.root.join("niructl"))
        .env("NDX_FX", &s.root)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    s.app = Some(app);
    let start = Instant::now();
    loop {
        let id = s.xdotool(&["search", "--name", "NIRU Display"]);
        if let Some(id) = id.lines().next().filter(|l| !l.is_empty()) {
            return id.to_string();
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no window");
        sleep(Duration::from_millis(100));
    }
}

fn count(log: &[String], line: &str) -> usize {
    log.iter().filter(|l| *l == line).count()
}

#[test]
#[ignore = "needs Xvfb, xdotool, xwd, xrandr and setxkbmap; CI runs it with --ignored"]
fn events_typing_preview_and_close_under_xvfb() {
    let mut s = start();
    // The keymap is Swedish before the window reads it; the window must
    // still follow a MappingNotify, which the second setxkbmap below sends.
    s.run("setxkbmap", &["us"]);
    open_window(&mut s);
    s.wait_log(Duration::from_secs(10), "the first status read", |l| {
        l.iter().any(|x| x == "display status --json")
    });
    sleep(Duration::from_secs(1));
    s.run("setxkbmap", &["se"]);
    sleep(Duration::from_millis(500));

    // D1: an idle window starts no process.
    let before = s.log().len();
    sleep(Duration::from_secs(10));
    assert_eq!(
        s.log().len(),
        before,
        "spawns while idle: {:?}",
        &s.log()[before..]
    );

    // D1: a RandR change made elsewhere is read again within a second.
    let reads = s.status_reads();
    s.run("xrandr", &["--fb", "1200x760"]);
    s.wait_log(Duration::from_secs(1), "the read after xrandr", |l| {
        count(l, "display status --json") > reads
    });
    sleep(Duration::from_millis(800));
    // ...once, and the window's own read does not start another one.
    let after = s.status_reads();
    assert_eq!(after, reads + 1, "one read per change: {:?}", s.log());
    sleep(Duration::from_secs(2));
    assert_eq!(s.status_reads(), after, "no reload loop");
    s.run("xrandr", &["--fb", "1280x800"]);
    s.wait_log(Duration::from_secs(1), "the read after the restore", |l| {
        count(l, "display status --json") > after
    });
    sleep(Duration::from_millis(800));

    // D2: "Save as…" with the Swedish keymap. xdotool presses the level's
    // modifiers itself: AltGr (ISO_Level3_Shift, Mod5) for @, ~ and $,
    // Shift for (, ), _ and Å.
    // Ctrl+S opens "Save as…".
    s.key(&["ctrl+s"]);
    s.key(&[
        "H",
        "e",
        "m",
        "m",
        "a",
        "space",
        "parenleft",
        "d",
        "o",
        "c",
        "k",
        "parenright",
        "underscore",
        "Aring",
        "at",
        "space",
        "dead_tilde",
        "dollar",
    ]);
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the typed save", |l| {
        l.iter().any(|x| x.starts_with("saved ["))
    });
    assert!(
        s.log().iter().any(|x| x == "saved [Hemma (dock)_Å@ ~$]"),
        "typed name: {:?}",
        s.log()
    );
    assert!(
        s.log()
            .iter()
            .any(|x| x == "display save-json Hemma (dock)_Å@ ~$"),
        "argv: {:?}",
        s.log()
    );
    let saved = fs::read_to_string(s.root.join("profiles/Hemma (dock)_Å@ ~$.json")).unwrap();
    assert!(saved.contains("\"name\":\"Hemma (dock)_Å@ ~$\""), "{saved}");
    sleep(Duration::from_millis(800));

    // D3: "Save as…" under the loaded profile's own name updates it
    // without asking...
    s.key(&["ctrl+s", "Return"]);
    s.wait_log(Duration::from_secs(5), "the update", |l| {
        count(l, "saved [Hemma (dock)_Å@ ~$]") == 2
    });
    sleep(Duration::from_millis(500));
    // ...while another existing name asks first, and Esc keeps it.
    s.key(&["ctrl+s"]);
    s.key(&["BackSpace"; 20]);
    s.key(&["W", "o", "r", "k", "Return"]);
    sleep(Duration::from_millis(500));
    assert!(
        !s.log().iter().any(|x| x == "saved [Work]"),
        "saved without asking"
    );
    s.key(&["Escape"]);
    sleep(Duration::from_millis(300));
    assert!(!s.log().iter().any(|x| x == "saved [Work]"), "Esc saved");
    s.key(&["ctrl+s"]);
    s.key(&["BackSpace"; 20]);
    s.key(&["W", "o", "r", "k", "Return", "Return"]);
    s.wait_log(Duration::from_secs(5), "the confirmed replace", |l| {
        l.iter().any(|x| x == "saved [Work]")
    });
    sleep(Duration::from_millis(500));

    // U2: during a preview started here, Enter keeps and Esc reverts.
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the first preview", |l| {
        l.iter().filter(|x| x.starts_with("previewed ")).count() == 1
    });
    sleep(Duration::from_millis(500));
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "Enter = Keep", |l| {
        l.iter().any(|x| x.starts_with("done keep "))
    });
    sleep(Duration::from_millis(500));
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the second preview", |l| {
        l.iter().filter(|x| x.starts_with("previewed ")).count() == 2
    });
    sleep(Duration::from_millis(500));
    s.key(&["Escape"]);
    s.wait_log(Duration::from_secs(5), "Esc = Revert", |l| {
        l.iter().any(|x| x.starts_with("done revert "))
    });
    assert!(!s.exited(), "Esc closed instead of reverting");
    sleep(Duration::from_millis(500));

    // D1: a preview that nobody keeps runs out; the window reads the status
    // just after its local deadline and sees the watchdog's revert, with
    // no reads every second in between.
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the third preview", |l| {
        l.iter().filter(|x| x.starts_with("previewed ")).count() == 3
    });
    let reads = s.status_reads();
    s.wait_log(Duration::from_secs(18), "the expiry", |l| {
        l.iter().any(|x| x.starts_with("expired "))
    });
    let during = s.status_reads() - reads;
    assert!(
        during <= 5,
        "{during} status reads during a 15 s preview (5 s recheck + expiry)"
    );
    sleep(Duration::from_millis(800));

    // D9: closing during a preview asks; Keep layout keeps and closes.
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the preview", |l| {
        l.iter().filter(|x| x.starts_with("previewed ")).count() == 4
    });
    sleep(Duration::from_millis(500));
    // Q closes (Esc would revert a preview started here, see U2).
    s.xdotool(&["key", "q"]);
    sleep(Duration::from_millis(500));
    assert!(!s.exited(), "Q closed the window during a preview");
    assert_eq!(
        s.log().iter().filter(|x| x.starts_with("done ")).count(),
        2,
        "closing kept or reverted without asking: {:?}",
        s.log()
    );
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the keep on close", |l| {
        l.iter().filter(|x| x.starts_with("done keep ")).count() == 2
    });
    let closed = Instant::now();
    while !s.exited() {
        assert!(closed.elapsed() < Duration::from_secs(3), "did not close");
        sleep(Duration::from_millis(25));
    }
    let window = open_window(&mut s);
    s.wait_log(Duration::from_secs(10), "the second window", |l| {
        count(l, "display status --json") >= 2
    });
    sleep(Duration::from_millis(800));

    // D5: a slow preview does not block repainting, and Esc still closes.
    fs::write(s.root.join("slow"), "4").unwrap();
    s.key(&["Return"]);
    s.wait_log(Duration::from_secs(5), "the slow preview", |l| {
        l.iter().any(|x| x.starts_with("display preview-json"))
    });
    sleep(Duration::from_millis(300));
    let working = s.screenshot(&window);
    s.xdotool(&["mousemove", "--window", &window, "200", "200"]);
    sleep(Duration::from_millis(400));
    let hovered = s.screenshot(&window);
    assert!(!working.is_empty() && !hovered.is_empty());
    let _ = hovered;
    let pressed = Instant::now();
    s.key(&["Escape"]);
    while !s.exited() {
        assert!(
            pressed.elapsed() < Duration::from_secs(2),
            "Esc did not close the window during a slow preview"
        );
        sleep(Duration::from_millis(25));
    }
}
