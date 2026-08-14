use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::{
        Arc, Mutex,
        mpsc::{self, TryRecvError},
    },
    thread,
    time::Instant,
};

use evdev::{Device, EventSummary, EventType, InputEvent, uinput::VirtualDevice};
use eyre::eyre;
use tracing::{debug, error};

use crate::{constants::*, gesture::TouchGestureInhibitor};

pub(crate) struct KeyboardHandler {
    keyboard_dev: Device,
    keyboard_uinput_dev: Option<Arc<Mutex<VirtualDevice>>>,

    key_tx: mpsc::Sender<(u16, bool)>,

    /// For "lockable" (or sticky) keys, stores when each of them was last pressed.
    last_lockable_key_presses: HashMap<u16, Instant>,

    /// The set of all keys that are currently "locked" down
    locked_keys: HashSet<u16>,
}

impl KeyboardHandler {
    pub fn start(
        keyboard_dev: Device,
        keyboard_uinput_dev: Option<Arc<Mutex<VirtualDevice>>>,
    ) -> impl TouchGestureInhibitor {
        let (key_tx, key_rx) = mpsc::channel();

        let handler = KeyboardHandler {
            keyboard_dev,
            keyboard_uinput_dev,
            key_tx,
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
            key_rx,
            keyboard_state: HashMap::new(),
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
                    self.key_tx.send((code.code(), value == 1))?;

                    // If we have a keyboard uinput dev it means we have keyboard features enabled
                    // Currently it just means a couple keys can be double-clicked to get their states locked
                    if let Some(ref mut keyboard_uinput_dev) = self.keyboard_uinput_dev {
                        // Clear all locked keys if a conflicting key is pressed or released
                        if KEYBOARD_LOCKED_KEYS_CONFLICTS.contains(&code.code())
                            && !self.locked_keys.is_empty()
                        {
                            for locked_key in self.locked_keys.drain() {
                                keyboard_uinput_dev
                                    .lock()
                                    .unwrap()
                                    .emit(&[InputEvent::new(EventType::KEY.0, locked_key, 0)])
                                    .ok();
                            }
                        }

                        // Now emit the event as-is
                        keyboard_uinput_dev.lock().unwrap().emit(&[kev.into()]).ok();

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
                                    .lock()
                                    .unwrap()
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
    /// State tracker of all keys on the keyboard, true = down
    keyboard_state: HashMap<u16, bool>,
    key_rx: mpsc::Receiver<(u16, bool)>,
}

impl KeyboardTouchInhibitor {
    fn handle_key(&mut self, key: u16, state: bool) {
        self.keyboard_state.insert(key, state);
    }

    fn should_inhibit(&self) -> bool {
        self.keyboard_state.values().any(|v| *v)
    }
}

impl TouchGestureInhibitor for KeyboardTouchInhibitor {
    fn next_should_inhibit(&mut self) -> eyre::Result<bool> {
        let (key, state) = self.key_rx.recv()?;
        self.handle_key(key, state);

        // Handle any additiona key events we can handle immediately
        loop {
            match self.key_rx.try_recv() {
                Ok((key, state)) => self.handle_key(key, state),
                Err(TryRecvError::Disconnected) => return Err(eyre!("disconnected, aborting")),
                Err(TryRecvError::Empty) => break,
            }
        }

        Ok(self.should_inhibit())
    }
}

/// When KEYBOARD_FEATURES is off we never open TitanKey. Park forever so the
/// gesture detector's inhibit thread stays idle (no pad-vs-type inhibit).
pub(crate) struct NoKeyboardInhibitor;

impl TouchGestureInhibitor for NoKeyboardInhibitor {
    fn next_should_inhibit(&mut self) -> eyre::Result<bool> {
        loop {
            thread::park();
        }
    }
}
