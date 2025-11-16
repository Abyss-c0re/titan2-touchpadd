use std::sync::atomic::Ordering;

use evdev::{
    Device, EventType, InputEvent, KeyCode, RelativeAxisCode, SynchronizationCode,
    uinput::VirtualDevice,
};
use tracing::{debug, info, warn};

use crate::{
    gesture::{Gesture, GestureDetector, SwipeGesture},
    keyboard::KeyboardHandler,
};

pub(crate) fn run_evloop(
    touchpad_dev: Device,
    keyboard_dev: Device,
    mut uinput_dev: VirtualDevice,
    keyboard_uinput_dev: Option<VirtualDevice>,
) -> eyre::Result<()> {
    let reject_flag = KeyboardHandler::start(keyboard_dev, keyboard_uinput_dev);
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
