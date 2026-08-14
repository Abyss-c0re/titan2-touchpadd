use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, TrySendError},
    },
    thread,
    time::{Duration, SystemTime},
};

use evdev::{AbsoluteAxisCode, Device};
use eyre::eyre;
use tracing::{debug, error, info, warn};

use crate::{
    constants::*,
    state::{TouchState, TouchStateTracker},
};

pub(crate) trait TouchGestureInhibitor: Send {
    fn next_should_inhibit(&mut self) -> eyre::Result<bool>;
}

#[derive(Debug)]
pub(crate) enum Gesture {
    /// X, Y coords
    PointerMove(i32, i32),
    /// Just a click
    Click,
    /// A long click
    LongClick,
    /// Start of a drag
    DragStart,
    /// End of a drag
    DragEnd,
    /// Vertical scrolling
    VerticalScroll(i32),
    /// Swiping
    Swipe(SwipeGesture),
    /// One step of Android text caret (KEY_LEFT=-1 / KEY_RIGHT=+1), not mouse pointer.
    CaretStep(i32),
    /// Double-tap on the top row of keys
    TopRowDoubleTap(u32),
}

#[derive(Debug)]
pub(crate) enum SwipeGesture {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Default)]
enum GestureInhibitionStatus {
    /// No inhibition whatsoever
    #[default]
    Normal,
    /// Actively inhibited
    Inhibited,
    /// Not inhibited anymore, but we still need to see a finger up event to fully cancel inhibition
    WaitingForUp,
}

#[derive(Clone, Default)]
struct GestureInhibition(Arc<Mutex<GestureInhibitionStatus>>);

impl GestureInhibition {
    fn should_inhibit(&self, ev: Option<&TouchState>) -> bool {
        let mut lock = self.0.lock().unwrap();
        match *lock {
            GestureInhibitionStatus::Normal => false,
            // Even if inhibited, if we see a finger up event, we can cancel touch inhibition
            // This is because sometimes the finger up event can happen before the corresponding keyboard
            // key up event.
            GestureInhibitionStatus::WaitingForUp | GestureInhibitionStatus::Inhibited => {
                if let Some(ev) = ev
                    && !ev.down
                {
                    *lock = GestureInhibitionStatus::Normal;
                }

                // Always block the last up event even if we ended up transitioning back to normal
                // This helps prevent spurious single-clicks in case keyboard / touch is somewhat de-synced
                // (e.g. we see the full key down -> up event sequence before we see any touch event)
                // since returning true here also causes GestureDetector to reset the state
                true
            }
        }
    }

    fn inhibit(&self) {
        *self.0.lock().unwrap() = GestureInhibitionStatus::Inhibited;
    }

    fn uninhibit(&self) {
        *self.0.lock().unwrap() = GestureInhibitionStatus::WaitingForUp
    }
}

pub(crate) struct GestureDetector {
    keyboard_features_enabled: bool,
    /// Independent of KEYBOARD_FEATURES / TitanKey: top-row strip drives cursor.
    /// When on, main pad surface is the lower region only (OEM top-row split).
    top_row_cursor: bool,
    /// Only top-row cursor path (no lower pad pointer/click) — standalone module.
    top_row_only: bool,
    inhibition: GestureInhibition,
    event_rx: mpsc::Receiver<eyre::Result<TouchState>>,
    gesture_tx: mpsc::SyncSender<eyre::Result<Gesture>>,
    last_touch: Option<TouchState>,
    first_down: Option<TouchState>,
    max_x: i32,
    max_y: i32,
    /// Absolute values of deltaX / Y, accumulated since the last down event
    /// This is used to detect long-tap-to-right-click
    delta_x_abs_acc: u32,
    delta_y_abs_acc: u32,
    /// Same but not absolute values
    delta_x_acc: i32,
    delta_y_acc: i32,
    /// If true, we have seen a single click and are waiting for further
    /// events to decide whether this is "just" a single click or the start
    /// of a double-click-and-drag gesture
    single_click_pending: bool,
    /// If true, we have seen the second down event for a double tap on the top row, and are waiting
    /// for the next up.
    top_row_double_tap_pending: bool,
    dragging: bool,
    /// Has a long-click been emitted for the current streak of touch down events?
    long_click_emitted: bool,
    /// Accumulated horizontal motion on top-row for text-caret steps.
    caret_accum_x: i32,
}

impl GestureDetector {
    pub(crate) fn start<I: 'static + TouchGestureInhibitor>(
        keyboard_features_enabled: bool,
        touchpad_dev: Device,
        mut inhibitor: I,
    ) -> eyre::Result<impl Iterator<Item = eyre::Result<Gesture>>> {
        // First acquire some basic properties of the device
        let mut max_x = -1;
        let mut max_y = -1;
        for (axis, info) in touchpad_dev.get_absinfo()? {
            if axis == AbsoluteAxisCode::ABS_MT_POSITION_X {
                max_x = info.maximum();
            } else if axis == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                max_y = info.maximum()
            }
        }

        if max_x == -1 || max_y == -1 {
            return Err(eyre!("No max X / Y coordinates available"));
        }

        // TOP_ROW_CURSOR: independent of KEYBOARD_FEATURES / TitanKey grab.
        // Default on so product mouse + HID paths get OEM top-row cursor without dual TitanKey.
        let top_row_cursor = env_flag_default_on("TOP_ROW_CURSOR");
        // TOP_ROW_ONLY: standalone module — only top-row cursor strip (no lower pad).
        let top_row_only = env_flag_default_off("TOP_ROW_ONLY");
        let top_row_cursor = top_row_cursor || top_row_only;

        info!(
            "Touch device max_x={max_x} max_y={max_y}; TOP_ROW_CURSOR={top_row_cursor} TOP_ROW_ONLY={top_row_only} PAD_SURFACE={}",
            if top_row_cursor { "lower" } else { "full" }
        );

        let (event_tx, event_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let tracker = TouchStateTracker::new(touchpad_dev);

            for event in tracker {
                if let Err(TrySendError::Disconnected(_)) = event_tx.try_send(event) {
                    // Terminate if the receiving end is dropped
                    break;
                }
            }
        });

        let inhibition = GestureInhibition::default();
        let _inhibition = inhibition.clone();

        thread::spawn(move || {
            while let Ok(inhibit) = inhibitor.next_should_inhibit() {
                if inhibit {
                    debug!("Touch inhibition started!");
                    _inhibition.inhibit();
                } else {
                    debug!("Touch inhibition ended! But might still need a touch up event!");
                    _inhibition.uninhibit();
                }
            }
        });

        let (gesture_tx, gesture_rx) = mpsc::sync_channel(16);

        let state = GestureDetector {
            keyboard_features_enabled,
            top_row_cursor,
            top_row_only,
            inhibition,
            event_rx,
            gesture_tx,
            last_touch: None,
            first_down: None,
            delta_x_abs_acc: 0,
            delta_y_abs_acc: 0,
            delta_x_acc: 0,
            delta_y_acc: 0,
            single_click_pending: false,
            top_row_double_tap_pending: false,
            dragging: false,
            long_click_emitted: false,
            caret_accum_x: 0,
            max_x,
            max_y,
        };

        thread::spawn(|| {
            if let Err(e) = state.run() {
                error!("Gesture detection loop exitted abnormally: {e:?}");
            }
        });

        Ok(gesture_rx.into_iter())
    }

    #[inline]
    fn top_row_y_max(&self) -> i32 {
        ((self.max_y as f64) * KEYBOARD_TOP_ROW_HEIGHT) as i32
    }

    #[inline]
    fn is_top_row_y(&self, y: i32) -> bool {
        y < self.top_row_y_max()
    }

    /// Contact started in the reserved top-row strip (shift/sym/back/recents/fn/alt).
    fn first_down_in_top_row(&self) -> bool {
        self.first_down
            .as_ref()
            .map(|t| self.is_top_row_y(t.y))
            .unwrap_or(false)
    }

    fn run(mut self) -> eyre::Result<()> {
        loop {
            // If we just had a single click, then either it's the start of
            // a double-click-to-drag gesture, or it's "just" a single click
            // We need a timeout here because if it is a single click, we need to
            // be able to emit the single click event without too much delay.
            let timeout = if self.single_click_pending {
                DOUBLE_CLICK_DRAG_TIMEOUT
            } else if self.first_down.is_some() {
                // In this case, this may be a long click, so we also need to be able to "wake up" in case nothing happens
                LONG_CLICK_DURATION
            } else {
                Duration::from_secs(86400)
            };

            let touch = self.event_rx.recv_timeout(timeout);

            if self
                .inhibition
                .should_inhibit(touch.as_ref().ok().and_then(|inner| inner.as_ref().ok()))
            {
                debug!("Input still inhibited!");
                // We also need to drop any pending clicks here
                self.reset_state(true)?;
                continue;
            }

            let touch = match touch {
                Ok(Ok(touch)) => {
                    if SystemTime::now()
                        .duration_since(touch.timestamp)
                        .unwrap_or(INVALID_DURATION)
                        > Duration::from_secs(1)
                    {
                        warn!("Received event that's way too old, ignoring");
                        continue;
                    } else {
                        touch
                    }
                }
                Ok(Err(e)) => {
                    self.emit(Err(e))?;
                    continue;
                }
                // Timed out after a single click, meaning that it truly was just a single click (not a double-click-to-drag)
                Err(RecvTimeoutError::Timeout) if self.single_click_pending => {
                    self.single_click_pending = false;
                    // Now emit a single click
                    self.emit(Ok(Gesture::Click))?;
                    continue;
                }
                // Timed out after the first down event, we may have a long click
                Err(RecvTimeoutError::Timeout) if self.first_down.is_some() => {
                    // We do still need to verify the delta x and y accumulators to ensure we haven't moved much
                    if self.no_significant_movement_since_down() && !self.long_click_emitted {
                        self.long_click_emitted = true;
                        self.emit(Ok(Gesture::LongClick))?;
                    }

                    continue;
                }
                // Unknown error, break the loop
                Err(e) => return Err(e.into()),
            };

            if self.single_click_pending {
                if touch
                    .timestamp
                    .duration_since(self.last_touch.as_ref().unwrap().timestamp)
                    .unwrap_or(INVALID_DURATION)
                    >= DOUBLE_CLICK_DRAG_TIMEOUT
                {
                    // Just a single click
                    self.single_click_pending = false;
                    // No click on top-row strip when reserved for cursor / keys
                    if !(self.top_row_cursor && self.is_top_row_y(touch.y)) && !self.top_row_only {
                        self.emit(Ok(Gesture::Click))?;
                    }
                } else if touch.down {
                    if (self.keyboard_features_enabled || self.top_row_cursor)
                        && self.is_top_row_y(touch.y)
                    {
                        // Double-tap on the top row of keys
                        self.top_row_double_tap_pending = true;
                    } else if !self.top_row_only {
                        // Drag started
                        self.dragging = true;
                        self.emit(Ok(Gesture::DragStart))?;
                    }
                }
            }

            // Reset the flag -- we don't need it if we got here at all
            self.single_click_pending = false;

            // The finger has been down for a while, could be a long click or just regular pointer movement
            if touch.down
                && let Some(ref last_touch) = self.last_touch
                && last_touch.down
                && let Some(ref first_down) = self.first_down
            {
                let top_row_stroke = self.top_row_cursor && self.is_top_row_y(first_down.y);

                if !top_row_stroke
                    && !self.top_row_only
                    && touch
                        .timestamp
                        .duration_since(first_down.timestamp)
                        .unwrap_or(INVALID_DURATION)
                        >= LONG_CLICK_DURATION
                    && self.no_significant_movement_since_down()
                    && !self.long_click_emitted
                {
                    // This is a long click (never on reserved top-row strip)
                    self.long_click_emitted = true;
                    self.emit(Ok(Gesture::LongClick))?;
                } else {
                    let delta_x = touch.x - last_touch.x;
                    let delta_y = touch.y - last_touch.y;

                    self.delta_x_abs_acc += delta_x.abs() as u32;
                    self.delta_y_abs_acc += delta_y.abs() as u32;
                    self.delta_x_acc += delta_x;
                    self.delta_y_acc += delta_y;

                    if top_row_stroke {
                        // Text caret (blinking insert point), not mouse pointer:
                        // accumulate horizontal travel → KEY_LEFT/RIGHT steps.
                        // Discrete fast swipe still emits Swipe on lift.
                        if self.try_detect_swipe(touch.timestamp).is_none() {
                            self.caret_accum_x += delta_x;
                            while self.caret_accum_x >= CARET_STEP_PX {
                                self.caret_accum_x -= CARET_STEP_PX;
                                self.emit(Ok(Gesture::CaretStep(1)))?;
                            }
                            while self.caret_accum_x <= -CARET_STEP_PX {
                                self.caret_accum_x += CARET_STEP_PX;
                                self.emit(Ok(Gesture::CaretStep(-1)))?;
                            }
                        }
                    } else if self.top_row_only {
                        // Standalone top-row only: ignore lower-surface strokes
                    } else if (first_down.x as f64) < self.max_x as f64 * SCROLL_EDGE_VERTICAL_THRESHOLD
                        || ((self.max_x - first_down.x) as f64)
                            < self.max_x as f64 * SCROLL_EDGE_VERTICAL_THRESHOLD
                    {
                        // This is vertical scroll (left or right edge)
                        self.emit(Ok(Gesture::VerticalScroll(delta_y)))?;
                    } else if self.try_detect_swipe(touch.timestamp).is_none() {
                        // Main pad surface: when top-row cursor is on, only lower area
                        // (first_down already below top row here).
                        self.emit(Ok(Gesture::PointerMove(delta_x, delta_y)))?;
                    }
                }
            }

            // The finger just tapped once (down then up before SINGLE_CLICK_TIMEOUT)
            // but we can't emit a single click just yet because this may be the start of a
            // double-click-to-drag.
            if let Some(ref first_down) = self.first_down
                && !touch.down
                && !self.dragging
                && !self.top_row_double_tap_pending
                && !self.top_row_only
                && !(self.top_row_cursor && self.is_top_row_y(first_down.y))
                && self.no_significant_movement_since_down()
                && self.try_detect_swipe(touch.timestamp).is_none()
                && touch
                    .timestamp
                    .duration_since(first_down.timestamp)
                    .unwrap_or(INVALID_DURATION)
                    < SINGLE_CLICK_TIMEOUT
            {
                self.single_click_pending = true;
            }

            if self.dragging && !touch.down {
                self.dragging = false;
                self.emit(Ok(Gesture::DragEnd))?;
            }

            self.last_touch = Some(touch.clone());

            if touch.down {
                if self.first_down.is_none() {
                    self.first_down = Some(touch.clone());
                }
            } else {
                if let Some(swipe) = self.try_detect_swipe(touch.timestamp) {
                    self.emit(Ok(Gesture::Swipe(swipe)))?;
                } else if self.top_row_double_tap_pending
                    && self.keyboard_features_enabled
                    && self.is_top_row_y(touch.y)
                {
                    // Numeric double-tap still requires KEYBOARD_FEATURES (virtual key inject).
                    let mut key_idx = None;
                    for i in 0..KEYBOARD_TOP_ROW_KEY_BOUNDS.len() {
                        let lower_bound = if i == 0 {
                            0
                        } else {
                            KEYBOARD_TOP_ROW_KEY_BOUNDS[i - 1]
                        };
                        let upper_bound = KEYBOARD_TOP_ROW_KEY_BOUNDS[i];
                        // The equal case is required on both ends because we might have an x coordinate of 0 or 1440
                        if touch.x >= lower_bound && touch.x <= upper_bound {
                            key_idx = Some(i as u32);
                        }
                    }
                    if let Some(idx) = key_idx {
                        self.emit(Ok(Gesture::TopRowDoubleTap(idx)))?
                    }
                }

                self.reset_state(false)?;
            }
        }
    }

    /// Emits a gesture event or error, but skips if the gesture event channel is already full.
    /// Returns an error if trhe gesture event channel is closed
    fn emit(&self, gesture_or_err: eyre::Result<Gesture>) -> eyre::Result<()> {
        if gesture_or_err.is_ok() {
            if let Err(TrySendError::Disconnected(_)) = self.gesture_tx.try_send(gesture_or_err) {
                return Err(eyre!("channel closed, shutting down"));
            }
        } else {
            self.gesture_tx.send(gesture_or_err)?;
        }

        Ok(())
    }

    fn reset_state(&mut self, clear_single_click: bool) -> eyre::Result<()> {
        if self.dragging {
            // if we're resetting state, we also need to tell everyone that we have stopped dragging
            // because dragging is a state that needs to be synchronized with consumers
            self.dragging = false;
            self.emit(Ok(Gesture::DragEnd))?;
        }

        self.first_down = None;
        self.delta_x_abs_acc = 0;
        self.delta_y_abs_acc = 0;
        self.delta_x_acc = 0;
        self.delta_y_acc = 0;
        self.long_click_emitted = false;
        self.top_row_double_tap_pending = false;
        self.caret_accum_x = 0;

        // This flag is set if we truly want to clear _all_ state, for example, when input inhibition
        // is triggered. It is _not_ set when we just want to reset most of the state when the finger lifts.
        if clear_single_click {
            self.single_click_pending = false;
        }

        Ok(())
    }

    fn no_significant_movement_since_down(&self) -> bool {
        (self.delta_x_abs_acc as f64) < self.max_x as f64 * NO_MOVEMENT_THRESHOLD
            && (self.delta_y_abs_acc as f64) < self.max_y as f64 * NO_MOVEMENT_THRESHOLD
    }

    fn try_detect_swipe(&self, now: SystemTime) -> Option<SwipeGesture> {
        if self.dragging {
            return None;
        }

        let Some(ref first_down) = self.first_down else {
            return None;
        };

        // Top-row cursor: L/R swipe only when contact starts on top-row keys.
        // KEYBOARD_FEATURES: OEM full-surface swipe → arrow keys (legacy).
        let top_row_stroke = self.top_row_cursor && self.is_top_row_y(first_down.y);
        if !top_row_stroke && !self.keyboard_features_enabled {
            return None;
        }
        if top_row_stroke && !self.top_row_cursor {
            return None;
        }

        let Ok(dur) = now.duration_since(first_down.timestamp) else {
            return None;
        };

        if dur >= SWIPE_DURATION {
            return None;
        }

        // We really want the same speed to apply to both X and Y directions,
        // so choose the wider direction as baseline
        let speed_ref = std::cmp::max(self.max_x, self.max_y) as f64;
        if dur.as_secs_f64() <= 0.0 {
            return None;
        }

        let speed_x = (self.delta_x_acc as f64 / dur.as_secs_f64()) / speed_ref;

        let res_x = if speed_x > SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Right)
        } else if speed_x < -SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Left)
        } else {
            None
        };

        // Top-row cursor only cares about horizontal (cursor left/right).
        if top_row_stroke {
            return res_x;
        }

        let speed_y = (self.delta_y_acc as f64 / dur.as_secs_f64()) / speed_ref;

        let res_y = if speed_y > SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Down)
        } else if speed_y < -SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Up)
        } else {
            None
        };

        match (res_x, res_y) {
            (None, None) => None,
            (Some(res_x), None) => Some(res_x),
            (None, Some(res_y)) => Some(res_y),
            (Some(res_x), Some(res_y)) => {
                // Choose the direction with bigger absolute speed
                if speed_x.abs() > speed_y.abs() {
                    Some(res_x)
                } else {
                    Some(res_y)
                }
            }
        }
    }
}

fn env_flag_default_on(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        }
        Err(_) => true,
    }
}

fn env_flag_default_off(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            v == "1" || v == "true" || v == "on" || v == "yes"
        }
        Err(_) => false,
    }
}
