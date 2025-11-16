use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use evdev::{Device, EventSummary, EventType, InputEvent, uinput::VirtualDevice};
use tracing::debug;

use crate::constants::*;

pub(crate) fn run_keyboard_loop(
    mut keyboard_dev: Device,
    mut keyboard_uinput_dev: Option<VirtualDevice>,
) -> Arc<AtomicBool> {
    // Flags used to inhibit touch around key press (used even if keyboard remap features aren't enabled)
    let flag = Arc::new(AtomicBool::new(false));
    let _flag = flag.clone();

    let (key_tx, key_rx) = mpsc::sync_channel(16);

    thread::spawn(move || {
        let mut last_lockable_key_presses = HashMap::new();
        let mut locked_keys = HashSet::new();

        for key in KEYBOARD_LOCKABLE_KEYS {
            last_lockable_key_presses.insert(key, Instant::now());
        }

        while let Ok(events) = keyboard_dev.fetch_events() {
            for ev in events {
                if let EventSummary::Key(kev, code, value) = ev.destructure() {
                    // Tell the touch side to reject input events for a while
                    key_tx.try_send(()).ok();

                    // If we have a keyboard uinput dev it means we have keyboard features enabled
                    // Currently it just means a couple keys can be double-clicked to get their states locked
                    if let Some(ref mut keyboard_uinput_dev) = keyboard_uinput_dev {
                        // Clear all locked keys if a conflicting key is pressed or released
                        if KEYBOARD_LOCKED_KEYS_CONFLICTS.contains(&code.code())
                            && !locked_keys.is_empty()
                        {
                            for locked_key in locked_keys.drain() {
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
                                last_lockable_key_presses.entry(code.code())
                        {
                            let now = Instant::now();

                            if now.duration_since(*last_press.get()) < KEYBOARD_DOUBLE_PRESS_TIMEOUT
                            {
                                // "Lock" the Fn key. The next press will natually cancel this.
                                debug!("Key {} locked!", code.code());
                                locked_keys.insert(code.code());
                                keyboard_uinput_dev
                                    .emit(&[InputEvent::new(EventType::KEY.0, code.code(), 1)])
                                    .ok();
                            } else {
                                *last_press.get_mut() = now;
                                // Also, we should not consider this key "locked" now
                                // (Note that the precondition of this whole branch is value == 0, so either it's the start
                                //  of a locked state, or the key isn't locked at all)
                                locked_keys.remove(&code.code());
                            }
                        }
                    }
                }
            }
        }
    });

    thread::spawn(move || {
        loop {
            let timeout = if _flag.load(Ordering::Relaxed) {
                KEYBOARD_TOUCH_REJECTION_TIMEOUT
            } else {
                Duration::from_secs(86400)
            };

            match key_rx.recv_timeout(timeout) {
                Ok(_) => {
                    debug!("Keyboard event received, rejecting touch for a while");
                    _flag.store(true, Ordering::Relaxed)
                }
                Err(RecvTimeoutError::Timeout) => _flag.store(false, Ordering::Relaxed),
                Err(_) => break,
            }
        }
    });

    return flag;
}
