use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use evdev::{
    Device, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDevice,
};
use tracing::{debug, info, warn};

use crate::{
    constants::NUMERIC_KEYCODES,
    gesture::{Gesture, GestureDetector, SwipeGesture},
    keyboard::{KeyboardHandler, NoKeyboardInhibitor},
    pause::PadPauseGate,
};

const KEY_ACTIVITY_FILES: &[&str] = &[
    "/data/local/tmp/titan2_key_activity",
    "/data/misc/titan2/titan2_key_activity",
];

fn emit_left(dev: &mut VirtualDevice, down: bool) -> eyre::Result<()> {
    let v = if down { 1 } else { 0 };
    dev.emit(&[
        InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, v),
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ),
    ])?;
    Ok(())
}

/// mtime:body signature for HW key activity plane (pad-agent / key-watch).
fn key_activity_sig() -> String {
    let mut best_mt = 0u64;
    let mut best_body = String::new();
    for p in KEY_ACTIVITY_FILES {
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
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let body = raw.trim().trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if body.len() < 10 || !body.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if mt >= best_mt {
            best_mt = mt;
            best_body = body.to_string();
        }
    }
    if best_body.is_empty() {
        String::new()
    } else {
        format!("{best_mt}:{best_body}")
    }
}

fn key_activity_fresh(sig: &str) -> bool {
    if sig.is_empty() {
        return false;
    }
    let Some(body) = sig.split(':').nth(1) else {
        return false;
    };
    let Ok(n) = body.parse::<u64>() else {
        return false;
    };
    let act_s = if body.len() >= 11 { n / 1000 } else { n };
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    wall.saturating_sub(act_s) <= 3
}

/// Hot plane: titan2_pad_tap_click / long_click / scroll / dbltap
const TAP_CLICK_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_tap_click",
    "/data/local/tmp/titan2_pad_tap_click",
];
const LONG_CLICK_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_long_click",
    "/data/local/tmp/titan2_pad_long_click",
];
const SCROLL_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_scroll",
    "/data/local/tmp/titan2_pad_scroll",
];
const DBLTAP_FILES: &[&str] = &[
    "/data/misc/titan2/titan2_pad_dbltap",
    "/data/local/tmp/titan2_pad_dbltap",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DblTap {
    Classic,
    Latch,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GestureCfg {
    tap: bool,
    long_click: bool,
    scroll: bool,
    dbltap: DblTap,
}

impl Default for GestureCfg {
    fn default() -> Self {
        Self {
            tap: true,
            long_click: true,
            scroll: true,
            dbltap: DblTap::Classic,
        }
    }
}

fn plane_bool(paths: &[&str], default: bool) -> bool {
    for p in paths {
        if let Ok(raw) = fs::read_to_string(p) {
            let s = raw
                .trim()
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_ascii_lowercase();
            if s.is_empty() {
                continue;
            }
            return matches!(s.as_str(), "1" | "true" | "on" | "yes");
        }
    }
    default
}

fn plane_dbltap() -> DblTap {
    for p in DBLTAP_FILES {
        if let Ok(raw) = fs::read_to_string(p) {
            let s = raw
                .trim()
                .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                .to_ascii_lowercase();
            if s.is_empty() {
                continue;
            }
            return match s.as_str() {
                "latch" | "hold" | "toggle" => DblTap::Latch,
                "off" | "0" | "none" | "false" => DblTap::Off,
                _ => DblTap::Classic,
            };
        }
    }
    DblTap::Classic
}

fn read_gesture_cfg(env_tap_master: bool) -> GestureCfg {
    if !env_tap_master {
        return GestureCfg {
            tap: false,
            long_click: false,
            scroll: plane_bool(SCROLL_FILES, true),
            dbltap: DblTap::Off,
        };
    }
    GestureCfg {
        tap: plane_bool(TAP_CLICK_FILES, true),
        long_click: plane_bool(LONG_CLICK_FILES, true),
        scroll: plane_bool(SCROLL_FILES, true),
        dbltap: plane_dbltap(),
    }
}

pub(crate) fn run_evloop(
    keyboard_features_enabled: bool,
    touchpad_dev: Device,
    keyboard_dev: Option<Device>,
    uinput_dev: VirtualDevice,
    keyboard_uinput_dev: Option<Arc<Mutex<VirtualDevice>>>,
    text_nav_dev: Option<Arc<Mutex<VirtualDevice>>>,
) -> eyre::Result<()> {
    // Split paths: with TitanKey (features on) vs pad-only (no open).
    if let Some(keyboard_dev) = keyboard_dev {
        let inhibitor = KeyboardHandler::start(keyboard_dev, keyboard_uinput_dev.clone());
        return run_evloop_inner(
            keyboard_features_enabled,
            touchpad_dev,
            inhibitor,
            uinput_dev,
            keyboard_uinput_dev,
            text_nav_dev,
        );
    }
    let inhibitor = NoKeyboardInhibitor;
    run_evloop_inner(
        keyboard_features_enabled,
        touchpad_dev,
        inhibitor,
        uinput_dev,
        keyboard_uinput_dev,
        text_nav_dev,
    )
}

fn emit_key_pulse(dev: &mut VirtualDevice, key: KeyCode) -> eyre::Result<()> {
    dev.emit(&[
        InputEvent::new(EventType::KEY.0, key.code(), 1),
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ),
        InputEvent::new(EventType::KEY.0, key.code(), 0),
        InputEvent::new(
            EventType::SYNCHRONIZATION.0,
            SynchronizationCode::SYN_REPORT.0,
            0,
        ),
    ])?;
    Ok(())
}

/// Android text caret (blinking insert point) — KEY_LEFT / KEY_RIGHT.
/// Prefer dedicated titan2-text-nav uinput (never grabs TitanKey).
fn emit_caret_step(
    step: i32,
    text_nav: &Option<Arc<Mutex<VirtualDevice>>>,
    keyboard_uinput: &Option<Arc<Mutex<VirtualDevice>>>,
) -> eyre::Result<()> {
    let key = if step < 0 {
        KeyCode::KEY_LEFT
    } else if step > 0 {
        KeyCode::KEY_RIGHT
    } else {
        return Ok(());
    };
    if let Some(dev) = text_nav {
        emit_key_pulse(&mut dev.lock().unwrap(), key)?;
        return Ok(());
    }
    if let Some(dev) = keyboard_uinput {
        emit_key_pulse(&mut dev.lock().unwrap(), key)?;
    }
    Ok(())
}

fn run_evloop_inner(
    keyboard_features_enabled: bool,
    touchpad_dev: Device,
    inhibitor: impl 'static + crate::gesture::TouchGestureInhibitor,
    mut uinput_dev: VirtualDevice,
    keyboard_uinput_dev: Option<Arc<Mutex<VirtualDevice>>>,
    text_nav_dev: Option<Arc<Mutex<VirtualDevice>>>,
) -> eyre::Result<()> {
    let detector = GestureDetector::start(keyboard_features_enabled, touchpad_dev, inhibitor)?;

    info!("Main event loop started");

    // Gesture policy: env TAP_TO_CLICK (legacy master) + plane files
    // titan2_pad_tap_click / long_click / scroll / dbltap (hot-read each gesture).
    let env_tap = match std::env::var("TAP_TO_CLICK") {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        }
        Err(_) => true,
    };
    if !env_tap {
        info!("Tap-to-click disabled (TAP_TO_CLICK env)");
    }

    // In-process park: drop REL/BTN while typing plane / cool active.
    // Process stays alive — pad-agent must not kill for typing lock.
    let mut pause_gate = PadPauseGate::new();
    pause_gate.refresh(); // status file before first gesture
    let mut drag_held = false;
    // Optional latch mode (titan2_pad_dbltap=latch): hold BTN_LEFT after double
    // tap until another double-tap or HW key. Default = classic drag-to-hold.
    let mut left_latch = false;
    let mut last_key_sig = key_activity_sig();
    let mut last_gesture_cfg = GestureCfg::default();

    for gesture in detector {
        pause_gate.refresh();
        let gcfg = read_gesture_cfg(env_tap);
        if gcfg != last_gesture_cfg {
            info!(
                "pad gestures tap={} long={} scroll={} dbltap={:?}",
                gcfg.tap, gcfg.long_click, gcfg.scroll, gcfg.dbltap
            );
            last_gesture_cfg = gcfg;
        }
        // HW keyboard press clears left latch (product: type cancels drag-lock).
        let ks = key_activity_sig();
        if left_latch && ks != last_key_sig && key_activity_fresh(&ks) {
            info!("left latch OFF (hw key activity)");
            let _ = emit_left(&mut uinput_dev, false);
            left_latch = false;
            drag_held = false;
        }
        last_key_sig = ks;

        let Ok(gesture) = gesture.inspect_err(|e| warn!("Could not construct touch state from events, ignoring the current SYN_REPORT: {:?}", e)) else {
            continue;
        };

        debug!("Gesture acquired: {:?}", gesture);

        if pause_gate.is_paused() {
            // Release stuck drag / latch so UI does not keep primary button down.
            if drag_held || left_latch {
                let _ = emit_left(&mut uinput_dev, false);
                drag_held = false;
                left_latch = false;
            }
            // Still allow caret/top-row text nav? Product: park = no pad pointer;
            // caret on top row during typing is optional — drop all pad output.
            continue;
        }

        match gesture {
            Gesture::PointerMove(delta_x, delta_y) => {
                debug!("Pointer move, deltaX={delta_x}, deltaY={delta_y}");
                uinput_dev.emit(&[
                    InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, delta_x),
                    InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, delta_y),
                ])?;
            }
            Gesture::Click if gcfg.tap => {
                if left_latch {
                    debug!("Left click ignored (latch held)");
                } else {
                    debug!("Left click!");
                    uinput_dev.emit(&[
                        InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 1),
                        InputEvent::new(
                            EventType::SYNCHRONIZATION.0,
                            SynchronizationCode::SYN_REPORT.0,
                            0,
                        ),
                        InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 0),
                    ])?;
                }
            }
            Gesture::LongClick if gcfg.long_click => {
                debug!("Right click!");
                uinput_dev.emit(&[
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_RIGHT.0, 1),
                    InputEvent::new(
                        EventType::SYNCHRONIZATION.0,
                        SynchronizationCode::SYN_REPORT.0,
                        0,
                    ),
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_RIGHT.0, 0),
                ])?;
            }
            Gesture::DragStart if gcfg.dbltap != DblTap::Off => {
                if gcfg.dbltap == DblTap::Latch {
                    // Optional: toggle hold until 2nd double-tap or key.
                    if left_latch {
                        info!("left latch OFF (double-tap)");
                        emit_left(&mut uinput_dev, false)?;
                        left_latch = false;
                        drag_held = false;
                    } else {
                        info!("left latch ON (double-tap hold)");
                        emit_left(&mut uinput_dev, true)?;
                        left_latch = true;
                        drag_held = true;
                        last_key_sig = key_activity_sig();
                    }
                } else {
                    // Classic: button down while finger stays after double-tap.
                    info!("double-tap drag start (classic hold)");
                    emit_left(&mut uinput_dev, true)?;
                    drag_held = true;
                    left_latch = false;
                }
            }
            Gesture::DragEnd if gcfg.dbltap != DblTap::Off => {
                if left_latch {
                    // Latch mode: finger-up does not release.
                    debug!("Drag end ignored (left latch held)");
                } else if drag_held {
                    debug!("Drag ended (classic)");
                    emit_left(&mut uinput_dev, false)?;
                    drag_held = false;
                }
            }
            Gesture::Click | Gesture::LongClick | Gesture::DragStart | Gesture::DragEnd => {}
            Gesture::VerticalScroll(val) if gcfg.scroll => {
                debug!("Vertical scroll!");
                uinput_dev.emit(&[InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_WHEEL_HI_RES.0,
                    val,
                )])?;
            }
            Gesture::VerticalScroll(_) => {
                debug!("Vertical scroll ignored (pad_scroll off)");
            }
            Gesture::CaretStep(step) => {
                debug!("Text caret step {step}");
                emit_caret_step(step, &text_nav_dev, &keyboard_uinput_dev)?;
            }
            Gesture::Swipe(swipe) => {
                debug!("Swipe! {:?}", swipe);
                // Top-row L/R → text caret (KEY_LEFT/RIGHT), never mouse REL.
                match swipe {
                    SwipeGesture::Left => {
                        emit_caret_step(-1, &text_nav_dev, &keyboard_uinput_dev)?;
                    }
                    SwipeGesture::Right => {
                        emit_caret_step(1, &text_nav_dev, &keyboard_uinput_dev)?;
                    }
                    SwipeGesture::Up | SwipeGesture::Down => {
                        // Only with full KEYBOARD_FEATURES virtual TitanKey
                        if let Some(ref keyboard_uinput_dev) = keyboard_uinput_dev {
                            let key = match swipe {
                                SwipeGesture::Up => KeyCode::KEY_UP,
                                SwipeGesture::Down => KeyCode::KEY_DOWN,
                                _ => unreachable!(),
                            };
                            emit_key_pulse(&mut keyboard_uinput_dev.lock().unwrap(), key)?;
                        }
                    }
                }
            }
            Gesture::TopRowDoubleTap(i) => {
                debug!("Double-tapping top row!");
                let key = NUMERIC_KEYCODES[i as usize];
                if let Some(keyboard_uinput_dev) = keyboard_uinput_dev.as_ref() {
                    emit_key_pulse(&mut keyboard_uinput_dev.lock().unwrap(), key)?;
                }
            }
        }
    }
    Ok(())
}
