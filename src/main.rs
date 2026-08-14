use std::{
    os::unix::fs::FileTypeExt,
    sync::{Arc, Mutex},
};

use evdev::{AttributeSet, Device, KeyCode, RelativeAxisCode, uinput};
use eyre::{OptionExt, eyre};
use tracing::{error, info, level_filters::LevelFilter, warn};
use tracing_logcat::{LogcatMakeWriter, LogcatTag};
use tracing_subscriber::{EnvFilter, fmt::format::Format};

use crate::constants::NUMERIC_KEYCODES;

mod constants;
mod evloop;
mod gesture;
mod keyboard;
mod pause;
mod state;

fn main() -> eyre::Result<()> {
    let base_subscriber = tracing_subscriber::fmt().with_env_filter(
        EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy(),
    );

    if let Ok(o) = std::env::var("LOGCAT_OUTPUT")
        && o == "true"
    {
        let tag = LogcatTag::Fixed(env!("CARGO_PKG_NAME").to_owned());
        let writer = LogcatMakeWriter::new(tag).expect("Failed to initialize logcat writer");

        base_subscriber
            .with_writer(writer)
            .event_format(Format::default().with_level(false).without_time())
            .with_ansi(false)
            .init();
    } else {
        base_subscriber.init();
    }

    info!("Detecting Titan 2's touchpad input...");
    // Version / SoT markers for pad-agent assert.
    info!("PAD_SURFACE tip: TEXT_CARET_NAV KEY_LEFT/RIGHT + lower pad surface (titanus2)");
    info!("INPROC_PARK tip: pause plane + key cool (no kill for typing)");

    // Features that remap / simulate keyboard input are gated.
    // When false (titanus2 hybrid default): never open TitanKey — that fights
    // hid_bridge exclusive EVIOCGRAB (S-HID-08 / C-TOUCHPADD-KEYBOARD).
    // Text-caret top-row swipe is independent (TOP_ROW_CURSOR / TEXT_CARET_NAV).
    let keyboard_features_enabled =
        matches!(std::env::var("KEYBOARD_FEATURES"), Ok(v) if v == "true");
    // Blinking insert-point nav on Shift/Sym/Back/Recents/Fn/Alt strip (default on).
    let text_caret_nav = match std::env::var("TEXT_CARET_NAV")
        .or_else(|_| std::env::var("TOP_ROW_CURSOR"))
    {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        }
        Err(_) => true,
    };
    let no_grab = matches!(
        std::env::var("TOP_ROW_NOGRAB").as_deref(),
        Ok("1") | Ok("true") | Ok("on") | Ok("yes")
    );

    let (Some(mut touchpad_dev), keyboard_dev) =
        find_touchpad_and_keyboard_dev(keyboard_features_enabled)?
    else {
        error!("No touchpad device found, exitting");
        return Err(eyre!("No touchpad device found"));
    };

    if keyboard_features_enabled && keyboard_dev.is_none() {
        error!("KEYBOARD_FEATURES=true but TitanKey not found");
        return Err(eyre!("No keyboard device found"));
    }

    info!("Creating virtual mouse input...");
    let uinput_axes = {
        let mut axes = AttributeSet::new();
        axes.insert(RelativeAxisCode::REL_X);
        axes.insert(RelativeAxisCode::REL_Y);
        axes.insert(RelativeAxisCode::REL_WHEEL_HI_RES);
        axes
    };

    let uinput_keys = {
        let mut keys = AttributeSet::new();
        keys.insert(KeyCode::BTN_LEFT);
        keys.insert(KeyCode::BTN_RIGHT);

        keys
    };

    let mut uinput_dev = uinput::VirtualDevice::builder()?
        .name("titan2-virtual-mouse")
        .with_relative_axes(&uinput_axes)?
        .with_keys(&uinput_keys)?
        .build()?;

    info!(
        "Virtual mouse input created at {}",
        uinput_dev
            .get_syspath()?
            .to_str()
            .ok_or_eyre("can't decode pathbuf")?
    );

    // Dedicated text-caret keyboard — never named TitanKey, never opens HW TitanKey.
    let text_nav_dev = if text_caret_nav {
        let mut keys = AttributeSet::new();
        keys.insert(KeyCode::KEY_LEFT);
        keys.insert(KeyCode::KEY_RIGHT);
        let mut dev = uinput::VirtualDevice::builder()?
            .name("titan2-text-nav")
            .with_keys(&keys)?
            .build()?;
        info!(
            "Text caret nav uinput at {} (KEY_LEFT/RIGHT for blinking insert point)",
            dev.get_syspath()?
                .to_str()
                .ok_or_eyre("can't decode pathbuf")?
        );
        Some(Arc::new(Mutex::new(dev)))
    } else {
        info!("TEXT_CARET_NAV off — no top-row caret swipe");
        None
    };

    let keyboard_uinput_dev = if keyboard_features_enabled {
        let Some(ref keyboard_dev) = keyboard_dev else {
            return Err(eyre!("keyboard required for KEYBOARD_FEATURES"));
        };
        let mut dev = uinput::VirtualDevice::builder()?
            .name("TitanKey") // For the keyboard we're really just adding minor features, so we reuse the official device name
            .with_relative_axes(&uinput_axes)?
            .with_keys(&{
                let mut keys = AttributeSet::new();
                for key in keyboard_dev.supported_keys().unwrap() {
                    keys.insert(key);
                }

                // These additional keys respond to touchpad gestures, but have to live on the keyboard
                // in order not to mess up Android's key charracter maps.
                keys.insert(KeyCode::KEY_LEFT);
                keys.insert(KeyCode::KEY_RIGHT);
                keys.insert(KeyCode::KEY_UP);
                keys.insert(KeyCode::KEY_DOWN);

                // Top-row virtual numeric keys
                for i in NUMERIC_KEYCODES {
                    keys.insert(i);
                }
                keys
            })?
            .build()?;

        info!(
            "Virtual keyboard created at {}",
            dev.get_syspath()?
                .to_str()
                .ok_or_eyre("can't decode pathbuf")?
        );

        Some(Arc::new(Mutex::new(dev)))
    } else {
        info!("KEYBOARD_FEATURES off — TitanKey left for Android / hid_bridge");
        None
    };

    // TOP_ROW_NOGRAB=1: share pad with Android trackpad (text-caret only path).
    if !no_grab {
        if let Err(e) = touchpad_dev.grab() {
            warn!(
                "Unable to grab touchpad device, continuing but there might be conflicts with system gestures: {e:?}"
            );
        }
    } else {
        info!("TOP_ROW_NOGRAB — not grabbing touchPad (trackpad coexistence)");
    }

    // Grab TitanKey only when we own keyboard features
    let mut keyboard_dev = keyboard_dev;
    if keyboard_uinput_dev.is_some() {
        if let Some(ref mut kbd) = keyboard_dev {
            if let Err(e) = kbd.grab() {
                return Err(eyre!(
                    "Unable to grab the keyboard device when keyboard features are enabled: {e:?}"
                ));
            }
        }
    }

    evloop::run_evloop(
        keyboard_features_enabled,
        touchpad_dev,
        keyboard_dev,
        uinput_dev,
        keyboard_uinput_dev,
        text_nav_dev,
    )
}

/// When `want_keyboard` is false, TitanKey is never opened (S-HID-08 / B4).
/// Name is read from sysfs first so we do not even briefly open TitanKey FDs
/// that race with hid_bridge EVIOCGRAB under exclusive keys=1.
fn find_touchpad_and_keyboard_dev(
    want_keyboard: bool,
) -> eyre::Result<(Option<Device>, Option<Device>)> {
    let mut touchpad_dev = None;
    let mut keyboard_dev = None;

    for ent in std::fs::read_dir("/dev/input")? {
        let Ok(ent) = ent else {
            continue;
        };

        let Ok(file_type) = ent.file_type() else {
            continue;
        };

        if !file_type.is_char_device() {
            continue;
        }

        let Ok(filename) = ent.file_name().into_string() else {
            continue;
        };

        if !filename.starts_with("event") {
            continue;
        }

        // Prefer sysfs name — never open devices we will not keep.
        let sysfs_name = std::fs::read_to_string(format!(
            "/sys/class/input/{filename}/device/name"
        ))
        .ok()
        .map(|s| s.trim().to_string());

        if let Some(ref name) = sysfs_name {
            if name == "TitanKey" && !want_keyboard {
                info!("Skipping TitanKey (KEYBOARD_FEATURES off) at /dev/input/{filename}");
                continue;
            }
            if name != "touchPad" && name != "TitanKey" {
                continue;
            }
        }

        info!("Checking device /dev/input/{filename}");

        let Ok(dev) = Device::open(ent.path()) else {
            warn!("Unable to open device /dev/input/{filename}, skipping");
            continue;
        };

        let name = dev
            .name()
            .map(|s| s.to_string())
            .or(sysfs_name);

        if let Some(name) = name {
            if name == "touchPad" {
                info!("Found touch pad device at /dev/input/{filename}");
                touchpad_dev = Some(dev);
            } else if name == "TitanKey" {
                if want_keyboard {
                    info!("Found keyboard device at /dev/input/{filename}");
                    keyboard_dev = Some(dev);
                } else {
                    info!("Skipping TitanKey (KEYBOARD_FEATURES off) at /dev/input/{filename}");
                    // drop dev — should be unreachable when sysfs worked
                }
            }
        }
    }

    Ok((touchpad_dev, keyboard_dev))
}
