use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use evdev::{
    Device, EventSummary, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDevice,
};
use tracing::{debug, info, warn};

use crate::gesture::{Gesture, GestureDetector};

// Every key press rejects touch events for this long
const KEYBOARD_TOUCH_REJECTION_TIMEOUT: Duration = Duration::from_millis(500);

fn run_keyboard_touch_rejection_loop(mut keyboard_dev: Device) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let _flag = flag.clone();

    let (key_tx, key_rx) = mpsc::sync_channel(16);

    thread::spawn(move || {
        while let Ok(events) = keyboard_dev.fetch_events() {
            for ev in events {
                // Any key event means we reject
                if let EventSummary::Key(_, _, _) = ev.destructure() {
                    key_tx.try_send(()).ok();
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
) -> eyre::Result<()> {
    let reject_flag = run_keyboard_touch_rejection_loop(keyboard_dev);
    let detector = GestureDetector::new(touchpad_dev)?;

    info!("Main event loop started");

    for gesture in detector.flatten() {
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
        }
    }
    Ok(())
}
