use std::{
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::Duration,
};

use evdev::Device;
use smallvec::{SmallVec, smallvec};

use crate::state::{TouchState, TouchStateTracker};

const SINGLE_CLICK_TIMEOUT: Duration = Duration::from_millis(100);
const DOUBLE_CLICK_DRAG_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(crate) enum Gesture {
    // X, Y coords
    PointerMove(i32, i32),
    // Left click
    LeftClick,
    // Start of a drag
    DragStart,
    // End of a drag
    DragEnd,
}

pub(crate) struct GestureDetector {
    event_rx: mpsc::Receiver<eyre::Result<TouchState>>,
    last_touch: Option<TouchState>,
    first_down: Option<TouchState>,
    // If true, we have seen a single click and are waiting for further
    // events to decide whether this is "just" a single click or the start
    // of a double-click-and-drag gesture
    single_click_pending: bool,
    dragging: bool,
}

impl GestureDetector {
    pub(crate) fn new(touchpad_dev: Device) -> GestureDetector {
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || {
            let tracker = TouchStateTracker::new(touchpad_dev);

            for event in tracker {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });

        GestureDetector {
            event_rx,
            last_touch: None,
            first_down: None,
            single_click_pending: false,
            dragging: false,
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
            } else {
                Duration::from_secs(86400)
            };

            let touch = match self.event_rx.recv_timeout(timeout) {
                Ok(Ok(touch)) => touch,
                Ok(Err(e)) => return Some(smallvec![Err(e)]),
                Err(RecvTimeoutError::Timeout) if self.single_click_pending => {
                    self.single_click_pending = false;
                    // Now emit a single click
                    return Some(smallvec![Ok(Gesture::LeftClick)]);
                }
                Err(_) => return None,
            };

            let mut yield_values: Self::Item = smallvec![];

            if self.single_click_pending {
                if touch
                    .timestamp
                    .duration_since(self.last_touch.as_ref().unwrap().timestamp)
                    .unwrap()
                    >= DOUBLE_CLICK_DRAG_TIMEOUT
                {
                    // Just a single click
                    self.single_click_pending = false;
                    yield_values.push(Ok(Gesture::LeftClick));
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
                let delta_x = touch.x - last_touch.x;
                let delta_y = touch.y - last_touch.y;
                yield_values.push(Ok(Gesture::PointerMove(delta_x, delta_y)));
            }

            if let Some(ref first_down) = self.first_down
                && !touch.down
                && !self.dragging
                && touch
                    .timestamp
                    .duration_since(first_down.timestamp)
                    .unwrap()
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
            }

            if !yield_values.is_empty() {
                return Some(yield_values);
            }
        }
    }
}
