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

use evdev::{
    Device, EventSummary, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDevice,
};
use tracing::{debug, info, warn};

use crate::{
    constants::*,
    gesture::{Gesture, GestureDetector, SwipeGesture},
};

fn run_keyboard_loop(
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

pub(crate) fn run_evloop(
    touchpad_dev: Device,
    keyboard_dev: Device,
    mut uinput_dev: VirtualDevice,
    keyboard_uinput_dev: Option<VirtualDevice>,
) -> eyre::Result<()> {
    let reject_flag = run_keyboard_loop(keyboard_dev, keyboard_uinput_dev);
    let detector = GestureDetector::start(touchpad_dev)?;

    info!("Main event loop started");

    for gesture in detector {
        if reject_flag.load(Ordering::Relaxed) {
            continue;
        }

        let Ok(gesture) = gesture.inspect_err(|e| warn!("Could not construct touch state from events, ignoring the current SYN_REPORT: {:?}", e)) else {
            continue;
        };

        debug!("Gesture acquired: {:?}", gesture);

        match gesture {
            Gesture::PointerMove(delta_x, delta_y) => {
                debug!("Pointer move, deltaX={delta_x}, deltaY={delta_y}");
                uinput_dev.emit(&[
                    InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, delta_x),
                    InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, delta_y),
                ])?;
            }
            Gesture::Click => {
                debug!("Left click!");
                uinput_dev.emit(&[
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 1),
                    // Need a SYN_REPORT in between to make sure it registers as a click (two separate states)
                    InputEvent::new(
                        EventType::SYNCHRONIZATION.0,
                        SynchronizationCode::SYN_REPORT.0,
                        0,
                    ),
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 0),
                ])?;
            }
            Gesture::LongClick => {
                debug!("Right click!");
                uinput_dev.emit(&[
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_RIGHT.0, 1),
                    // Need a SYN_REPORT in between to make sure it registers as a click (two separate states)
                    InputEvent::new(
                        EventType::SYNCHRONIZATION.0,
                        SynchronizationCode::SYN_REPORT.0,
                        0,
                    ),
                    InputEvent::new(EventType::KEY.0, KeyCode::BTN_RIGHT.0, 0),
                ])?;
            }
            Gesture::DragStart => {
                debug!("Drag started!");
                uinput_dev.emit(&[InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 1)])?;
            }
            Gesture::DragEnd => {
                debug!("Drag ended!");
                uinput_dev.emit(&[InputEvent::new(EventType::KEY.0, KeyCode::BTN_LEFT.0, 0)])?;
            }
            Gesture::VerticalScroll(val) => {
                debug!("Vertical scroll!");
                uinput_dev.emit(&[InputEvent::new(
                    EventType::RELATIVE.0,
                    RelativeAxisCode::REL_WHEEL_HI_RES.0,
                    val,
                )])?;
            }
            Gesture::Swipe(swipe) => {
                debug!("Swipe!");

                let key = match swipe {
                    SwipeGesture::Left => KeyCode::KEY_LEFT,
                    SwipeGesture::Right => KeyCode::KEY_RIGHT,
                    SwipeGesture::Up => KeyCode::KEY_UP,
                    SwipeGesture::Down => KeyCode::KEY_DOWN,
                };

                uinput_dev.emit(&[
                    InputEvent::new(EventType::KEY.0, key.code(), 1),
                    InputEvent::new(
                        EventType::SYNCHRONIZATION.0,
                        SynchronizationCode::SYN_REPORT.0,
                        0,
                    ),
                    InputEvent::new(EventType::KEY.0, key.code(), 0),
                ])?;
            }
        }
    }
    Ok(())
}
