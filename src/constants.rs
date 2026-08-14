use std::time::Duration;

use evdev::KeyCode;

/// Maximum time elapsed between a single pair of touch down - touch up events
/// which we would consider a single click.
pub const SINGLE_CLICK_TIMEOUT: Duration = Duration::from_millis(200);
/// Maximum time elapsed between the first single click and a subsequent tap
/// which we would consider the start of a drag gesture.
pub const DOUBLE_CLICK_DRAG_TIMEOUT: Duration = Duration::from_millis(200);
/// Minimum time elapsed after a touch down event without significant movement
/// which we would consider a long click.
pub const LONG_CLICK_DURATION: Duration = Duration::from_secs(1);

/// Maximum duration for a gesture to be recognized as a swipe.
pub const SWIPE_DURATION: Duration = Duration::from_millis(200);
/// How fast must the finger move to be recognized as a swipe.
/// The unit here is (percent. of longer edge) / sec.
pub const SWIPE_SPEED_THRESHOLD: f64 = 1.0;

pub const INVALID_DURATION: Duration = Duration::from_secs(u64::MAX);

/// The threshold (relative to maxmimum values) of what we consider "no movement"
/// Used for long-click detection
pub const NO_MOVEMENT_THRESHOLD: f64 = 0.005;

/// How much of each edge do we consider as the "scrolling" region
pub const SCROLL_EDGE_VERTICAL_THRESHOLD: f64 = 0.005;

/// How quickly does a key have to be pressed to be considered "double pressed"
pub const KEYBOARD_DOUBLE_PRESS_TIMEOUT: Duration = Duration::from_millis(200);

/// Which keys can be double pressed to get their state temporarily "locked"?
pub const KEYBOARD_LOCKABLE_KEYS: [u16; 4] = [
    251, // The custom FUNCTION key code of Unihertz Titan 2,
    253, // The custom SYM key code of Unihertz Titan 2,
    KeyCode::KEY_LEFTSHIFT.0,
    KeyCode::KEY_RIGHTALT.0,
];

/// Keys that conflict with the "locked" state; these keys don't really work
/// when one of [KEYBOARD_LOCKABLE_KEYS] are locked, so pressing them will
/// cancel the locked state.
pub const KEYBOARD_LOCKED_KEYS_CONFLICTS: [u16; 3] = [
    KeyCode::KEY_SPACE.0,
    KeyCode::KEY_BACKSPACE.0,
    KeyCode::KEY_ENTER.0,
];

/// The height of the keyboard's top row, as a percentage of total height.
/// When TOP_ROW_CURSOR is on, the main pad surface is the remaining lower area.
pub const KEYBOARD_TOP_ROW_HEIGHT: f64 = 0.25;

/// Pixels of horizontal top-row travel per KEY_LEFT / KEY_RIGHT (text caret).
pub const CARET_STEP_PX: i32 = 36;

/// Boundaries of keys on the top row of keyboard (shift/sym/back/recents/fn/alt).
/// The mapping from keyboard to touchpad isn't exactly linear, for whatever reason,
/// so here's my best attempt at making detecting the corresponding key somewhat usable.
pub const KEYBOARD_TOP_ROW_KEY_BOUNDS: [i32; 6] = [180, 450, 713, 994, 1254, 1440];

/// All numeric keys we know about, only up to [KEYBOARD_TOP_ROW_NUM_KEYS] will be used
pub const NUMERIC_KEYCODES: [KeyCode; 9] = [
    KeyCode::KEY_1,
    KeyCode::KEY_2,
    KeyCode::KEY_3,
    KeyCode::KEY_4,
    KeyCode::KEY_5,
    KeyCode::KEY_6,
    KeyCode::KEY_7,
    KeyCode::KEY_8,
    KeyCode::KEY_9,
];
