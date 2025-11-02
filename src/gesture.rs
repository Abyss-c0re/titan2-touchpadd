use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, SystemTime},
};

use evdev::Device;
use smallvec::{SmallVec, smallvec};
use tracing::warn;

use crate::state::{TouchState, TouchStateTracker};

const SINGLE_CLICK_TIMEOUT: Duration = Duration::from_millis(100);
const DOUBLE_CLICK_DRAG_TIMEOUT: Duration = Duration::from_millis(200);
const LONG_CLICK_DURATION: Duration = Duration::from_secs(1);

const INVALID_DURATION: Duration = Duration::from_secs(u64::MAX);

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
}

pub(crate) struct GestureDetector {
    event_rx: mpsc::Receiver<eyre::Result<TouchState>>,
    last_touch: Option<TouchState>,
    first_down: Option<TouchState>,
    /// Absolute values of deltaX / Y, accumulated since the last down event
    /// This is used to detect long-tap-to-right-click
    delta_x_abs_acc: u32,
    delta_y_abs_acc: u32,
    /// If true, we have seen a single click and are waiting for further
    /// events to decide whether this is "just" a single click or the start
    /// of a double-click-and-drag gesture
    single_click_pending: bool,
    dragging: bool,
    /// Has a long-click been emitted for the current streak of touch down events?
    long_click_emitted: bool,
}

impl GestureDetector {
    pub(crate) fn new(touchpad_dev: Device) -> GestureDetector {
        let (event_tx, event_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let tracker = TouchStateTracker::new(touchpad_dev);

            for event in tracker {
                if event_tx.send(event).is_err() {
                    // Terminate if the receiving end is dropped
                    break;
                }
            }
        });

        GestureDetector {
            event_rx,
            last_touch: None,
            first_down: None,
            delta_x_abs_acc: 0,
            delta_y_abs_acc: 0,
            single_click_pending: false,
            dragging: false,
            long_click_emitted: false,
        }
    }
}

impl Iterator for GestureDetector {
    type Item = SmallVec<[eyre::Result<Gesture>; 4]>;

    fn next(&mut self) -> Option<Self::Item> {
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

            let touch = match self.event_rx.recv_timeout(timeout) {
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
                Ok(Err(e)) => return Some(smallvec![Err(e)]),
                Err(RecvTimeoutError::Timeout) if self.single_click_pending => {
                    self.single_click_pending = false;
                    // Now emit a single click
                    return Some(smallvec![Ok(Gesture::Click)]);
                }
                Err(RecvTimeoutError::Timeout) if self.first_down.is_some() => {
                    // Nothing happened since the last down event, which means this may be a long click
                    if self.delta_x_abs_acc < 50
                        && self.delta_y_abs_acc < 50
                        && !self.long_click_emitted
                    {
                        self.long_click_emitted = true;
                        return Some(smallvec![Ok(Gesture::LongClick)]);
                    } else {
                        continue;
                    }
                }
                Err(_) => return None,
            };

            let mut yield_values: Self::Item = smallvec![];

            if self.single_click_pending {
                if touch
                    .timestamp
                    .duration_since(self.last_touch.as_ref().unwrap().timestamp)
                    .unwrap_or(INVALID_DURATION)
                    >= DOUBLE_CLICK_DRAG_TIMEOUT
                {
                    // Just a single click
                    self.single_click_pending = false;
                    yield_values.push(Ok(Gesture::Click));
                } else if touch.down {
                    // Drag started
                    self.dragging = true;
                    yield_values.push(Ok(Gesture::DragStart));
                }
            }

            // Reset the flag -- we don't need it if we got here at all
            self.single_click_pending = false;

            if touch.down
                && let Some(ref last_touch) = self.last_touch
                && last_touch.down
            {
                if let Some(ref first_down) = self.first_down
                    && touch
                        .timestamp
                        .duration_since(first_down.timestamp)
                        .unwrap_or(INVALID_DURATION)
                        >= LONG_CLICK_DURATION
                    && self.delta_x_abs_acc < 50
                    && self.delta_y_abs_acc < 50
                    && !self.long_click_emitted
                {
                    // This is a long click
                    self.long_click_emitted = true;
                    yield_values.push(Ok(Gesture::LongClick));
                } else {
                    let delta_x = touch.x - last_touch.x;
                    let delta_y = touch.y - last_touch.y;
                    yield_values.push(Ok(Gesture::PointerMove(delta_x, delta_y)));
                    self.delta_x_abs_acc += delta_x.abs() as u32;
                    self.delta_y_abs_acc += delta_y.abs() as u32;
                }
            }

            if let Some(ref first_down) = self.first_down
                && !touch.down
                && !self.dragging
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
                yield_values.push(Ok(Gesture::DragEnd));
            }

            self.last_touch = Some(touch.clone());

            if touch.down {
                if self.first_down.is_none() {
                    self.first_down = Some(touch.clone());
                }
            } else {
                self.first_down = None;
                // These states also need to be reset if the finger is lifted
                self.delta_x_abs_acc = 0;
                self.delta_y_abs_acc = 0;
                self.long_click_emitted = false;
            }

            if !yield_values.is_empty() {
                return Some(yield_values);
            }
        }
    }
}
