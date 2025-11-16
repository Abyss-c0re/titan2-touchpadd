use std::{
    sync::mpsc::{self, RecvTimeoutError, TrySendError},
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
    fn should_inhibit(&self, now: SystemTime) -> bool;
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
}

#[derive(Debug)]
pub(crate) enum SwipeGesture {
    Left,
    Right,
    Up,
    Down,
}

enum GestureInhibitionStatus {
    /// No inhibition whatsoever
    Normal,
    /// Actively inhibited
    Inhibited,
    /// Not inhibited anymore, but we still need to see a finger up event to fully cancel inhibition
    WaitingForUp,
}

pub(crate) struct GestureDetector<I: 'static + TouchGestureInhibitor> {
    inhibitor: I,
    inhibition_status: GestureInhibitionStatus,
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
    dragging: bool,
    /// Has a long-click been emitted for the current streak of touch down events?
    long_click_emitted: bool,
}

impl<I: 'static + TouchGestureInhibitor> GestureDetector<I> {
    pub(crate) fn start(
        touchpad_dev: Device,
        inhibitor: I,
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

        info!("Touch device has max_x = {max_x}, max_y = {max_y}, assuming minimums are 0");

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

        let (gesture_tx, gesture_rx) = mpsc::sync_channel(16);

        let state = GestureDetector {
            inhibitor,
            inhibition_status: GestureInhibitionStatus::Normal,
            event_rx,
            gesture_tx,
            last_touch: None,
            first_down: None,
            delta_x_abs_acc: 0,
            delta_y_abs_acc: 0,
            delta_x_acc: 0,
            delta_y_acc: 0,
            single_click_pending: false,
            dragging: false,
            long_click_emitted: false,
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

            let now = if let Ok(Ok(ref t)) = touch {
                t.timestamp
            } else {
                SystemTime::now()
            };

            if self.inhibitor.should_inhibit(now) {
                debug!("Touch temporarily inhibited, resetting state and ignoring");
                self.reset_state()?;
                self.inhibition_status = GestureInhibitionStatus::Inhibited;
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
                        match self.inhibition_status {
                            GestureInhibitionStatus::WaitingForUp => {
                                if touch.down {
                                    // Haven't seen an up event yet
                                    debug!("Still needs an up event to resume touch!");
                                    continue;
                                } else {
                                    debug!("Seen finger up! Fully cancelling inhibition");
                                    self.inhibition_status = GestureInhibitionStatus::Normal
                                }
                            }
                            GestureInhibitionStatus::Inhibited => {
                                if touch.down {
                                    // We're no longer being inhibited, but we need to observe an up event
                                    debug!("No longer inhibited, waiting for up event");
                                    self.inhibition_status = GestureInhibitionStatus::WaitingForUp;
                                    continue;
                                } else {
                                    debug!("No longer inhibited!");
                                    self.inhibition_status = GestureInhibitionStatus::Normal
                                }
                            }
                            _ => {}
                        }

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
                    self.emit(Ok(Gesture::Click))?;
                } else if touch.down {
                    // Drag started
                    self.dragging = true;
                    self.emit(Ok(Gesture::DragStart))?;
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
                if touch
                    .timestamp
                    .duration_since(first_down.timestamp)
                    .unwrap_or(INVALID_DURATION)
                    >= LONG_CLICK_DURATION
                    && self.no_significant_movement_since_down()
                    && !self.long_click_emitted
                {
                    // This is a long click
                    self.long_click_emitted = true;
                    self.emit(Ok(Gesture::LongClick))?;
                } else {
                    let delta_x = touch.x - last_touch.x;
                    let delta_y = touch.y - last_touch.y;

                    self.delta_x_abs_acc += delta_x.abs() as u32;
                    self.delta_y_abs_acc += delta_y.abs() as u32;
                    self.delta_x_acc += delta_x;
                    self.delta_y_acc += delta_y;

                    if (first_down.x as f64) < self.max_x as f64 * SCROLL_EDGE_VERTICAL_THRESHOLD
                        || ((self.max_x - first_down.x) as f64)
                            < self.max_x as f64 * SCROLL_EDGE_VERTICAL_THRESHOLD
                    {
                        // This is vertical scroll (left or right edge)
                        self.emit(Ok(Gesture::VerticalScroll(delta_y)))?;
                    } else if self.try_detect_swipe(touch.timestamp).is_none() {
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
                }

                self.reset_state()?;
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

    fn reset_state(&mut self) -> eyre::Result<()> {
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

        let Ok(dur) = now.duration_since(first_down.timestamp) else {
            return None;
        };

        if dur >= SWIPE_DURATION {
            return None;
        }

        // We really want the same speed to apply to both X and Y directions,
        // so choose the wider direction as baseline
        let speed_ref = std::cmp::max(self.max_x, self.max_y) as f64;

        let speed_x = (self.delta_x_acc as f64 / dur.as_secs_f64()) / speed_ref;

        let res_x = if speed_x > SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Right)
        } else if speed_x < -SWIPE_SPEED_THRESHOLD {
            Some(SwipeGesture::Left)
        } else {
            None
        };

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
