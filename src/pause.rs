//! In-process pad park (OPTIMIZE Phase 1).
//! Process stays up; drops REL/BTN while parked. No kill → no native ABS residual.
//!
//! Plane: docs/project/PAD_TOUCHPADD_CONTRACT.md
//! - titan2_pad_cursor_pause rising edge + key activity → cool window
//! - cool_ms from titan2_pad_cursor_cool_ms / pause_ms (100–5000, default 500)
//! - env PAUSE=1 forces park
//! Status: /data/local/tmp/titan2_touchpadd_status

use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tracing::{debug, info};

const STATUS_TP: &str = "/data/local/tmp/titan2_touchpadd_status";

const PAUSE_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_cursor_pause",
    "/data/local/tmp/titan2_pad_cursor_pause",
];
const COOL_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_cursor_cool_ms",
    "/data/local/tmp/titan2_pad_cursor_cool_ms",
    "/data/misc/titan2/titan2_pad_cursor_pause_ms",
    "/data/local/tmp/titan2_pad_cursor_pause_ms",
];
const ACTIVITY_FILES: &[&str] = &[
    "/data/local/tmp/titan2_key_activity",
    "/data/misc/titan2/titan2_key_activity",
];
const MODE_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_mode",
    "/data/local/tmp/titan2_pad_mode",
];

fn read_trim(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| {
            s.trim()
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

fn parse_bool_on(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "1" | "true" | "on" | "yes"
    )
}

fn read_cool_ms() -> u64 {
    for p in COOL_FILES {
        if let Some(s) = read_trim(p) {
            if let Ok(v) = s.parse::<u64>() {
                if v == 0 {
                    continue;
                }
                return v.clamp(100, 5000);
            }
        }
    }
    500
}

fn read_mode() -> String {
    for p in MODE_FILES {
        if let Some(s) = read_trim(p) {
            return s;
        }
    }
    "mouse".into()
}

fn activity_unix_s(raw: &str) -> Option<u64> {
    let n: u64 = raw.parse().ok()?;
    if raw.len() >= 11 {
        Some(n / 1000)
    } else {
        Some(n)
    }
}

fn wall_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct GateInner {
    cool: Duration,
    unlock_at: Option<Instant>,
    last_activity_sig: String,
    env_force: bool,
    plane_pause: bool,
    prev_plane_pause: bool,
    last_source: &'static str,
    mode: String,
    was_paused: bool,
}

impl GateInner {
    fn tick(&mut self) -> bool {
        let cool_ms = read_cool_ms();
        self.cool = Duration::from_millis(cool_ms);
        self.mode = read_mode();

        self.plane_pause = PAUSE_FILES.iter().any(|p| {
            read_trim(p)
                .map(|s| parse_bool_on(&s))
                .unwrap_or(false)
        });

        if self.plane_pause && !self.prev_plane_pause {
            self.unlock_at = Some(Instant::now() + self.cool);
            self.last_source = "plane";
            debug!(cool_ms, "plane pause rising edge → cool arm");
        }
        self.prev_plane_pause = self.plane_pause;

        let mut best_body = String::new();
        let mut best_mt = 0u64;
        for p in ACTIVITY_FILES {
            let path = Path::new(p);
            let Ok(meta) = fs::metadata(path) else {
                continue;
            };
            let mt = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let Some(body) = read_trim(p) else {
                continue;
            };
            if body.len() < 10 || !body.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if mt >= best_mt {
                best_mt = mt;
                best_body = body;
            }
        }

        if !best_body.is_empty() {
            let sig = format!("{best_mt}:{best_body}");
            if sig != self.last_activity_sig {
                self.last_activity_sig = sig;
                if let Some(act) = activity_unix_s(&best_body) {
                    let wall = wall_unix_s();
                    let age = wall.saturating_sub(act);
                    if age <= 3 {
                        self.unlock_at = Some(Instant::now() + self.cool);
                        self.last_source = "key";
                        debug!(age, cool_ms, "key activity re-arm cool");
                    }
                }
            }
        }

        if let Some(until) = self.unlock_at {
            if Instant::now() >= until {
                self.unlock_at = None;
            }
        }

        let paused = self.compute_paused();
        if paused != self.was_paused {
            self.was_paused = paused;
            if paused {
                info!(
                    source = self.last_source,
                    "pad park ON (in-process; no kill)"
                );
            } else {
                info!("pad park OFF (mouse emit live)");
            }
        }
        self.write_status(paused);
        paused
    }

    fn compute_paused(&self) -> bool {
        if self.env_force {
            return true;
        }
        if let Some(until) = self.unlock_at {
            if Instant::now() < until {
                return true;
            }
        }
        false
    }

    fn write_status(&self, park: bool) {
        let cool_left = self
            .unlock_at
            .map(|u| u.saturating_duration_since(Instant::now()).as_millis())
            .unwrap_or(0);
        let pid = std::process::id();
        let line = format!(
            "mode={} park={} pid={pid} cool_ms={} cool_left_ms={cool_left} source={} plane={} touchpadd=1\n",
            self.mode,
            if park { 1 } else { 0 },
            self.cool.as_millis(),
            self.last_source,
            if self.plane_pause { 1 } else { 0 },
        );
        if fs::write(STATUS_TP, &line).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(STATUS_TP, fs::Permissions::from_mode(0o666));
        }
    }
}

/// Shared gate: background poller updates AtomicBool; emit path only loads flag.
pub(crate) struct PadPauseGate {
    paused: Arc<AtomicBool>,
}

impl PadPauseGate {
    pub(crate) fn new() -> Self {
        let env_force = match std::env::var("PAUSE") {
            Ok(v) => parse_bool_on(&v),
            Err(_) => false,
        };
        if env_force {
            info!("Pad pause forced (PAUSE env)");
        }
        let cool_ms = read_cool_ms();
        info!(
            cool_ms,
            "PadPauseGate: in-process park (50ms poll; plane edge + key cool)"
        );

        let paused = Arc::new(AtomicBool::new(env_force));
        let paused_bg = Arc::clone(&paused);
        let inner = Arc::new(Mutex::new(GateInner {
            cool: Duration::from_millis(cool_ms),
            unlock_at: None,
            last_activity_sig: String::new(),
            env_force,
            plane_pause: false,
            prev_plane_pause: false,
            last_source: "boot",
            mode: read_mode(),
            was_paused: false,
        }));

        // Initial status
        if let Ok(mut g) = inner.lock() {
            let p = g.tick();
            paused.store(p, Ordering::Relaxed);
        }

        thread::Builder::new()
            .name("tp-pause".into())
            .spawn(move || {
                loop {
                    thread::sleep(Duration::from_millis(50));
                    let p = match inner.lock() {
                        Ok(mut g) => g.tick(),
                        Err(_) => continue,
                    };
                    paused_bg.store(p, Ordering::Relaxed);
                }
            })
            .ok();

        Self { paused }
    }

    pub(crate) fn refresh(&mut self) {
        // Poller owns refresh; no-op hot path.
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}
