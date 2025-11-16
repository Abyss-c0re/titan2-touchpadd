use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use evdev::{Device, EventSummary, EventType, InputEvent, uinput::VirtualDevice};
use tracing::{debug, error};

use crate::{constants::*, gesture::TouchGestureInhibitor};

pub(crate) struct KeyboardHandler {
    keyboard_dev: Device,
    keyboard_uinput_dev: Option<VirtualDevice>,

    program_epoch: SystemTime,

    /// Duration in millis since program_epoch
    last_key_press: Arc<AtomicU64>,

    /// For "lockable" (or sticky) keys, stores when each of them was last pressed.
    last_lockable_key_presses: HashMap<u16, Instant>,

    /// The set of all keys that are currently "locked" down
    locked_keys: HashSet<u16>,
}

impl KeyboardHandler {
    pub fn start(
        keyboard_dev: Device,
        keyboard_uinput_dev: Option<VirtualDevice>,
    ) -> impl TouchGestureInhibitor {
        let handler = KeyboardHandler {
            keyboard_dev,
            keyboard_uinput_dev,
            program_epoch: SystemTime::now(),
            last_key_press: Arc::new(AtomicU64::new(0)),
            last_lockable_key_presses: {
                let mut h = HashMap::new();
                for key in KEYBOARD_LOCKABLE_KEYS {
                    h.insert(key, Instant::now());
                }
                h
            },
            locked_keys: HashSet::new(),
        };

        let inhibitor = KeyboardTouchInhibitor {
            last_key_press: handler.last_key_press.clone(),
            program_epoch: handler.program_epoch,
        };

        thread::spawn(move || {
            if let Err(e) = handler.run() {
                error!("keyboard handler loop exitted abnormally: {e:?}");
            }
        });

        return inhibitor;
    }

    fn run(mut self) -> eyre::Result<()> {
        loop {
            for ev in self.keyboard_dev.fetch_events()? {
                if let EventSummary::Key(kev, code, value) = ev.destructure() {
                    // Tell the touch side to reject input events for a while
                    self.last_key_press.store(
                        ev.timestamp()
                            .duration_since(self.program_epoch)
                            .unwrap_or(INVALID_DURATION)
                            .as_millis() as u64,
                        Ordering::Relaxed,
                    );

                    // If we have a keyboard uinput dev it means we have keyboard features enabled
                    // Currently it just means a couple keys can be double-clicked to get their states locked
                    if let Some(ref mut keyboard_uinput_dev) = self.keyboard_uinput_dev {
                        // Clear all locked keys if a conflicting key is pressed or released
                        if KEYBOARD_LOCKED_KEYS_CONFLICTS.contains(&code.code())
                            && !self.locked_keys.is_empty()
                        {
                            for locked_key in self.locked_keys.drain() {
                                keyboard_uinput_dev
                                    .emit(&[InputEvent::new(EventType::KEY.0, locked_key, 0)])
                                    .ok();
                            }
                        }

                        // Now emit the event as-is
                        keyboard_uinput_dev.emit(&[kev.into()]).ok();

                        // If this key is part of the "lockable" set, check whether it has been pressed in quick succession
                        // If so, temporarily "lock" its state to pressed until the next press (that happens naturally)
                        if value == 0
                            && let Entry::Occupied(mut last_press) =
                                self.last_lockable_key_presses.entry(code.code())
                        {
                            let now = Instant::now();

                            if now.duration_since(*last_press.get()) < KEYBOARD_DOUBLE_PRESS_TIMEOUT
                            {
                                // "Lock" the Fn key. The next press will natually cancel this.
                                debug!("Key {} locked!", code.code());
                                self.locked_keys.insert(code.code());
                                keyboard_uinput_dev
                                    .emit(&[InputEvent::new(EventType::KEY.0, code.code(), 1)])
                                    .ok();
                            } else {
                                *last_press.get_mut() = now;
                                // Also, we should not consider this key "locked" now
                                // (Note that the precondition of this whole branch is value == 0, so either it's the start
                                //  of a locked state, or the key isn't locked at all)
                                self.locked_keys.remove(&code.code());
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) struct KeyboardTouchInhibitor {
    program_epoch: SystemTime,
    last_key_press: Arc<AtomicU64>,
}

impl TouchGestureInhibitor for KeyboardTouchInhibitor {
    fn should_inhibit(&self, now: SystemTime) -> bool {
        let last_key_press =
            self.program_epoch + Duration::from_millis(self.last_key_press.load(Ordering::Relaxed));

        now.duration_since(last_key_press)
            .unwrap_or(INVALID_DURATION)
            <= KEYBOARD_TOUCH_REJECTION_TIMEOUT
    }
}
