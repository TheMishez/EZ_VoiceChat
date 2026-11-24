//! Auto-updates from GitHub Releases (TheMishez/EZ_VoiceChat).
//!
//! Checks ~10 s after start and every 6 hours. When a newer release has the installer attached
//! ("…-setup.exe"), the app downloads it in the background and offers "Restart to update", which
//! runs it silently (EZ VoiceChat installs per-user, so no admin prompt) and starts the app again.
use crate::util;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

const OWNER: &str = "TheMishez";
const REPO: &str = "EZ_VoiceChat";

#[derive(Clone, Default)]
struct St {
    phase: String, // idle | checking | none | downloading | ready | error
    version: String,
    notes: String,
    percent: u32,
    error: String,
    file: Option<std::path::PathBuf>,
}

static ST: Mutex<Option<St>> = parking_lot::const_mutex(None);

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    let mut g = ST.lock();
    f(g.get_or_insert_with(|| St { phase: "idle".into(), ..Default::default() }))
}

fn page() -> String {
    format!("https://github.com/{OWNER}/{REPO}/releases/latest")
}

fn app_json() -> std::path::PathBuf {
    util::data_dir().join("app.json")
}

/// Stored with the page's other app options ("checkUpdates", default on).
pub fn enabled() -> bool {
    util::read_json(&app_json()).and_then(|v| v.get("checkUpdates").and_then(|b| b.as_bool())).unwrap_or(true)
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn state() -> Value {
    with(|s| {
        json!({
            "current": current(),
            "phase": s.phase,
            "version": s.version,
            "notes": s.notes,
            "percent": s.percent,
            "error": s.error,
            "enabled": enabled(),
            "page": page(),
        })
    })
}

fn changed() {
    util::emit("update-state", state());
}

fn parts(v: &str) -> Option<Vec<u64>> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+', ' ']).next()?;
    let p: Option<Vec<u64>> = core.split('.').map(|x| x.parse().ok()).collect();
    p.filter(|p| !p.is_empty())
}

fn newer(a: &[u64], b: &[u64]) -> bool {
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// The first time a new version runs, ask Windows to refresh icons, so desktop and Start menu
/// shortcuts show the current app icon instead of a cached old one.
fn refresh_icons_after_update() {
    let p = util::data_dir().join("last-version.txt");
    let last = std::fs::read_to_string(&p).unwrap_or_default();
    if last.trim() == current() {
        return;
    }
    let _ = std::fs::write(&p, current());
    if last.is_empty() {
        return; // fresh install: nothing cached yet
    }
    #[cfg(windows)]
    std::thread::spawn(|| unsafe {
        use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
        SHChangeNotify(SHCNE_ASSOCCHANGED as _, SHCNF_IDLIST, std::ptr::null(), std::ptr::null());
        let mut cmd = std::process::Command::new("ie4uinit.exe");
        cmd.arg("-show");
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
        let _ = cmd.status();
    });
}

pub fn init() {
    check_last_attempt();
    refresh_icons_after_update();
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        loop {
            if enabled() {
                check();
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

/// Looks for a newer release and, if there is one, starts downloading it.
pub fn check() -> Value {
    if with(|s| matches!(s.phase.as_str(), "checking" | "downloading" | "ready")) {
        return state();
    }
    with(|s| {
        s.phase = "checking".into();
        s.error.clear();
    });
    changed();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(12)).build();
    let res = agent
        .get(&format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest"))
        .set("User-Agent", &format!("EZ-VoiceChat/{}", current()))
        .set("Accept", "application/vnd.github+json")
        .call();
    let mut asset = String::new();
    match res {
        Ok(r) => {
            let j: Value = r.into_json().unwrap_or(Value::Null);
            let tag = j["tag_name"].as_str().unwrap_or("");
            let latest = parts(tag);
            let mine = parts(current());
            let pre = j["draft"].as_bool().unwrap_or(false) || j["prerelease"].as_bool().unwrap_or(false);
            let is_newer = matches!((&latest, &mine), (Some(l), Some(m)) if newer(l, m));
            if is_newer && !pre {
                asset = j["assets"]
                    .as_array()
                    .and_then(|a| {
                        a.iter().find_map(|x| {
                            let n = x["name"].as_str().unwrap_or("").to_lowercase();
                            let u = x["browser_download_url"].as_str().unwrap_or("");
                            (n.ends_with("-setup.exe") && u.starts_with("https://github.com/")).then(|| u.to_string())
                        })
                    })
                    .unwrap_or_default();
                let notes: String = util::strip_html(j["body"].as_str().unwrap_or("")).trim().chars().take(3000).collect();
                with(|s| {
                    s.version = latest.unwrap().iter().map(|n| n.to_string()).collect::<Vec<_>>().join(".");
                    s.notes = notes;
                    s.phase = if asset.is_empty() { "error".into() } else { "downloading".into() };
                    if asset.is_empty() {
                        s.error = "This release has no installer attached yet.".into();
                    }
                });
            } else {
                with(|s| {
                    s.phase = "none".into();
                    s.version.clear();
                });
            }
        }
        Err(ureq::Error::Status(404, _)) => with(|s| s.phase = "none".into()),
        Err(_) => with(|s| {
            s.phase = "error".into();
            s.error = "Couldn't reach GitHub to check for updates.".into();
        }),
    }
    changed();
    if !asset.is_empty() && !cfg!(debug_assertions) {
        download(asset);
    }
    state()
}

fn download(url: String) {
    with(|s| s.percent = 0);
    changed();
    std::thread::spawn(move || {
        let r = (|| -> Result<std::path::PathBuf, String> {
            let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).build();
            let resp = agent.get(&url).set("User-Agent", "EZ-VoiceChat").call().map_err(|e| e.to_string())?;
            let total: u64 = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut rd = resp.into_reader();
            let path = std::env::temp_dir().join(format!("EZ-VoiceChat-Setup-{}.exe", util::now_ms()));
            let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 64 * 1024];
            let (mut got, mut last) = (0u64, 0u32);
            loop {
                let n = rd.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut f, &buf[..n]).map_err(|e| e.to_string())?;
                got += n as u64;
                if total > 0 {
                    let p = ((got * 100) / total) as u32;
                    if p != last {
                        last = p;
                        with(|s| s.percent = p);
                        changed();
                    }
                }
            }
            if got < 1024 * 1024 {
                return Err("download too small".into());
            }
            Ok(path)
        })();
        match r {
            Ok(p) => with(|s| {
                s.phase = "ready".into();
                s.percent = 100;
                s.file = Some(p);
            }),
            Err(e) => {
                util::log(format!("update download failed: {e}"));
                with(|s| {
                    s.phase = "error".into();
                    s.error = "The update download failed. It will try again later.".into();
                });
            }
        }
        changed();
    });
}

fn pending_path() -> std::path::PathBuf {
    util::data_dir().join("update-pending.json")
}

/// After an update attempt: did it take? If the app is still on an older version, say so
/// instead of quietly offering the same update again.
fn check_last_attempt() {
    let Some(v) = util::read_json(&pending_path()) else { return };
    let _ = std::fs::remove_file(pending_path());
    let want = v["version"].as_str().unwrap_or("").to_string();
    match (parts(&want), parts(current())) {
        (Some(w), Some(m)) if newer(&w, &m) => {
            util::log(format!("update to {want} did not install (still {})", current()));
            with(|s| {
                s.phase = "error".into();
                s.version = want.clone();
                s.error = format!("The update to v{want} didn't finish installing. Download it from the releases page and run it.");
            });
        }
        _ => util::log(format!("updated to {}", current())),
    }
}

/// Runs the downloaded installer (a small progress window, no questions), then starts the app
/// again. A helper does this after the app has fully closed; it's started outside the app's
/// process group so closing the app can't take it down with it.
pub fn install() -> bool {
    let Some((file, version)) = with(|s| if s.phase == "ready" { s.file.clone().map(|f| (f, s.version.clone())) } else { None }) else { return false };
    let exe = std::env::current_exe().unwrap_or_default();
    let name = exe.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let log = util::data_dir().join("voicechat.log");
    util::write_json(&pending_path(), &json!({ "version": version }));
    util::log(format!("installing update {version} from {}", file.display()));
    let q = |p: &str| format!("'{}'", p.replace('\'', "''"));
    let ps = format!(
        "$ErrorActionPreference = 'Continue'; \
         $p = Get-Process -Id {pid} -ErrorAction SilentlyContinue; if ($p) {{ [void]$p.WaitForExit(20000) }}; \
         try {{ $i = Start-Process -FilePath {file} -ArgumentList '/P','/R' -Wait -PassThru; $code = $i.ExitCode }} catch {{ $code = $_.Exception.Message }}; \
         Add-Content -Path {log} -Value ('[update] installer finished: ' + $code); \
         Start-Sleep -Milliseconds 800; \
         if (-not (Get-Process -Name {name} -ErrorAction SilentlyContinue)) {{ Start-Process -FilePath {exe} }}",
        pid = std::process::id(),
        file = q(&file.to_string_lossy()),
        log = q(&log.to_string_lossy()),
        name = q(&name),
        exe = q(&exe.to_string_lossy()),
    );
    let spawn = |flags: u32| {
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", &ps]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(flags);
        }
        #[cfg(not(windows))]
        let _ = flags;
        cmd.spawn()
    };
    const NO_WINDOW: u32 = 0x0800_0000;
    const NEW_GROUP: u32 = 0x0000_0200;
    const BREAKAWAY: u32 = 0x0100_0000;
    let started = spawn(NO_WINDOW | NEW_GROUP | BREAKAWAY).or_else(|_| spawn(NO_WINDOW | NEW_GROUP));
    match started {
        Ok(_) => {
            crate::quit();
            true
        }
        Err(e) => {
            util::log(format!("could not start the update: {e}"));
            let _ = std::fs::remove_file(pending_path());
            with(|s| {
                s.phase = "error".into();
                s.error = "Couldn't start the installer. Download the update from the releases page.".into();
            });
            changed();
            false
        }
    }
}
