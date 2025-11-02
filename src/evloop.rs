use std::time::SystemTime;

use evdev::{
    AbsoluteAxisCode, Device, EventSummary, EventType, InputEvent, KeyCode, RelativeAxisCode,
    SynchronizationCode, uinput::VirtualDevice,
};
use eyre::eyre;
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
struct TouchState {
    x: i32,
    y: i32,
    down: bool,
    timestamp: SystemTime,
}

struct TouchStateTracker {
    inner: Device,
    last_state: TouchState,
    pending_events: Vec<EventSummary>,
}

impl TouchStateTracker {
    fn new(inner: Device) -> Self {
        Self {
            inner,
            last_state: TouchState {
                x: 0,
                y: 0,
                down: false,
                timestamp: SystemTime::now(),
            },
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
                    }
                    ret.timestamp = ev.timestamp();
                }
                EventSummary::Key(ev, code, state) => {
                    // Technically the touchpad emits both BTN_TOUCH and BTN_TOOL_FINGER, but we only
                    // use one here.
                    if *code == KeyCode::BTN_TOUCH {
                        ret.down = *state == 1;
                    }
                    ret.timestamp = ev.timestamp();
                    seen_btn = true;
                }
                _ => warn!("Ignoring unknown event {:?}", ev),
            }
        }

        if !(seen_btn || seen_x || seen_y) {
            Err(eyre!("Missing ABS_MT_ position events or BTN_TOUCH"))
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

pub(crate) fn run_evloop(
    mut touchpad_dev: Device,
    mut uinput_dev: VirtualDevice,
) -> eyre::Result<()> {
    let tracker = TouchStateTracker::new(touchpad_dev);

    let mut last_touch: Option<TouchState> = None;

    info!("Main event loop started");

    for touch in tracker {
        let Ok(touch) = touch.inspect_err(|e| warn!("Could not construct touch state from events, ignoring the current SYN_REPORT: {:?}", e)) else {
            continue;
        };

        debug!("Touch event acquired: {:?}", touch);

        if let Some(ref last_touch) = last_touch {
            if last_touch.down && touch.down {
                // Dragging pointer
                uinput_dev.emit(&[
                    InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_X.0,
                        touch.x - last_touch.x,
                    ),
                    InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_Y.0,
                        touch.y - last_touch.y,
                    ),
                ])?;
            }
        }

        last_touch = Some(touch);
    }
    Ok(())
}
