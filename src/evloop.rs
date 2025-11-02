use evdev::{
    Device, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDevice,
};
use tracing::{debug, info, warn};

use crate::gesture::{Gesture, GestureDetector};

pub(crate) fn run_evloop(touchpad_dev: Device, mut uinput_dev: VirtualDevice) -> eyre::Result<()> {
    let detector = GestureDetector::new(touchpad_dev);

    info!("Main event loop started");

    for gesture in detector.flatten() {
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
        }
    }
    Ok(())
}
