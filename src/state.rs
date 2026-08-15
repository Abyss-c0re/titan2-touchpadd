use std::time::SystemTime;

use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode, SynchronizationCode};
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
pub(crate) struct TouchState {
    pub x: i32,
    pub y: i32,
    pub down: bool,
    pub timestamp: SystemTime,
}

pub(crate) struct TouchStateTracker {
    inner: Device,
    last_state: TouchState,
    pending_events: Vec<EventSummary>,
}

impl TouchStateTracker {
    pub(crate) fn new(inner: Device) -> Self {
        // HID exclusive start while the finger is already down: BTN_TOUCH is
        // edge-triggered. Seed from EVIOCGKEY / EVIOCGABS so the first SYN
        // emits instead of waiting for lift + new down.
        let mut last_state = TouchState {
            x: 0,
            y: 0,
            down: false,
            timestamp: SystemTime::now(),
        };
        if let Ok(keys) = inner.get_key_state() {
            last_state.down = keys.contains(KeyCode::BTN_TOUCH)
                || keys.contains(KeyCode::BTN_TOOL_FINGER);
        }
        if let Ok(abs) = inner.get_absinfo() {
            let mut mx = None;
            let mut my = None;
            let mut sx = None;
            let mut sy = None;
            for (code, info) in abs {
                if code == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mx = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    my = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_X {
                    sx = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_Y {
                    sy = Some(info.value());
                }
            }
            last_state.x = mx.or(sx).unwrap_or(0);
            last_state.y = my.or(sy).unwrap_or(0);
        }
        if last_state.down {
            info!(
                x = last_state.x,
                y = last_state.y,
                "seed live contact (HID attach mid-touch)"
            );
        }
        Self {
            inner,
            last_state,
            pending_events: vec![],
        }
    }

    fn try_construct_touch_state(&mut self) -> eyre::Result<TouchState> {
        let mut ret = self.last_state.clone();
        let mut seen_x = false;
        let mut seen_y = false;
        let mut seen_btn = false;

        for ev in self.pending_events.iter() {
            match ev {
                EventSummary::AbsoluteAxis(ev, code, val) => {
                    if *code == AbsoluteAxisCode::ABS_MT_POSITION_X {
                        ret.x = *val;
                        seen_x = true;
                    } else if *code == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                        ret.y = *val;
                        seen_y = true;
                    } else if *code == AbsoluteAxisCode::ABS_MT_TRACKING_ID {
                        ret.down = *val >= 0;
                        seen_btn = true;
                    }
                    ret.timestamp = ev.timestamp();
                }
                EventSummary::Key(ev, code, state) => {
                    // Technically the touchpad emits both BTN_TOUCH and BTN_TOOL_FINGER, but we only
                    // use one here.
                    if *code == KeyCode::BTN_TOUCH || *code == KeyCode::BTN_TOOL_FINGER {
                        ret.down = *state == 1;
                    }
                    ret.timestamp = ev.timestamp();
                    seen_btn = true;
                }
                _ => warn!("Ignoring unknown event {:?}", ev),
            }
        }

        if !(seen_btn || seen_x || seen_y) {
            Err(eyre::eyre!("Missing ABS_MT_ position events or BTN_TOUCH"))
        } else {
            self.last_state = ret.clone();
            Ok(ret)
        }
    }
}

impl Iterator for TouchStateTracker {
    type Item = eyre::Result<TouchState>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // It's OK to call collect here since the evdev crate's iterator will always terminate on a SYN_REPORT
            let Ok(events) = self.inner.fetch_events().map(|ev| ev.collect::<Vec<_>>()) else {
                error!("Failed to fetch more events, terminating");
                return None;
            };

            for cur_event in events {
                let cur_event = cur_event.destructure();

                if let EventSummary::Synchronization(_, syn_code, _) = cur_event {
                    // This is what Titan 2's touchpad uses
                    if syn_code == SynchronizationCode::SYN_REPORT {
                        let ret = self.try_construct_touch_state();
                        debug!("Constructed touch state {ret:?}");
                        self.pending_events.clear();
                        return Some(ret);
                    }

                    // If we don't know about a synchronization event we still clear the pending events
                    self.pending_events.clear();
                } else {
                    self.pending_events.push(cur_event);
                }
            }
        }
    }
}
