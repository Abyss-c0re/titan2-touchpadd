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

pub const SWIPE_DURATION: Duration = Duration::from_millis(200);
pub const SWIPE_SPEED_THRESHOLD: f64 = 1.0;

pub const INVALID_DURATION: Duration = Duration::from_secs(u64::MAX);

/// The threshold (relative to maxmimum values) of what we consider "no movement"
/// Used for long-click detection
pub const NO_MOVEMENT_THRESHOLD: f64 = 0.005;

/// How much of each edge do we consider as the "scrolling" region
pub const SCROLL_EDGE_VERTICAL_THRESHOLD: f64 = 0.005;

/// Every key press rejects touch events for this long
pub const KEYBOARD_TOUCH_REJECTION_TIMEOUT: Duration = Duration::from_millis(500);

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
