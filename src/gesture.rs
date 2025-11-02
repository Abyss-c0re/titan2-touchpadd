use std::time::Duration;

use evdev::Device;

use crate::state::{TouchState, TouchStateTracker};

#[derive(Debug)]
pub(crate) enum Gesture {
    // X, Y coords
    PointerMove(i32, i32),
    // Left click
    LeftClick,
}

pub(crate) struct GestureDetector {
    tracker: TouchStateTracker,
    last_touch: Option<TouchState>,
    first_down: Option<TouchState>,
}

impl GestureDetector {
    pub(crate) fn new(touchpad_dev: Device) -> GestureDetector {
        GestureDetector {
            tracker: TouchStateTracker::new(touchpad_dev),
            last_touch: None,
            first_down: None,
        }
    }
}

impl Iterator for GestureDetector {
    type Item = eyre::Result<Gesture>;

    fn next(&mut self) -> Option<Self::Item> {
        for touch in &mut self.tracker {
            let touch = match touch {
                Ok(touch) => touch,
                Err(e) => return Some(Err(e)),
            };

            let mut yield_value: Option<Gesture> = None;

            if touch.down
                && let Some(ref last_touch) = self.last_touch
                && last_touch.down
            {
                let delta_x = touch.x - last_touch.x;
                let delta_y = touch.y - last_touch.y;
                yield_value = Some(Gesture::PointerMove(delta_x, delta_y));
            }

            if let Some(ref first_down) = self.first_down
                && !touch.down
                && touch
                    .timestamp
                    .duration_since(first_down.timestamp)
                    .unwrap()
                    < Duration::from_millis(100)
            {
                yield_value = Some(Gesture::LeftClick);
            }

            self.last_touch = Some(touch.clone());

            if touch.down {
                if self.first_down.is_none() {
                    self.first_down = Some(touch.clone());
                }
            } else {
                self.first_down = None;
            }

            if let Some(yield_value) = yield_value {
                return Some(Ok(yield_value));
            }
        }

        None
    }
}
