use std::os::unix::fs::FileTypeExt;

use evdev::{AttributeSet, Device, KeyCode, RelativeAxisCode, uinput};
use eyre::{OptionExt, eyre};
use tracing::{error, info, level_filters::LevelFilter, warn};
use tracing_logcat::{LogcatMakeWriter, LogcatTag};
use tracing_subscriber::{EnvFilter, fmt::format::Format};

mod constants;
mod evloop;
mod gesture;
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

    let (Some(mut touchpad_dev), Some(mut keyboard_dev)) = find_touchpad_and_keyboard_dev()? else {
        error!("No touchpad or keyboard devices found, exitting");
        return Err(eyre!("No touchpad device found"));
    };

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

    let keyboard_uinput_dev = if let Ok(v) = std::env::var("KEYBOARD_FEATURES")
        && v == "true"
    {
        let mut dev = uinput::VirtualDevice::builder()?
            .name("TitanKey") // For the keyboard we're really just adding minor features, so we reuse the official device name
            .with_relative_axes(&uinput_axes)?
            .with_keys(keyboard_dev.supported_keys().unwrap())?
            .build()?;

        info!(
            "Virtual keyboard created at {}",
            dev.get_syspath()?
                .to_str()
                .ok_or_eyre("can't decode pathbuf")?
        );

        Some(dev)
    } else {
        None
    };

    if let Err(e) = touchpad_dev.grab() {
        warn!(
            "Unable to grab touchpad device, continuing but there might be conflicts with system gestures: {e:?}"
        );
    }

    if keyboard_uinput_dev.is_some()
        && let Err(e) = keyboard_dev.grab()
    {
        return Err(eyre!(
            "Unable to grab the keyboard device when keyboard features are enabled: {e:?}"
        ));
    }

    evloop::run_evloop(touchpad_dev, keyboard_dev, uinput_dev, keyboard_uinput_dev)
}

fn find_touchpad_and_keyboard_dev() -> eyre::Result<(Option<Device>, Option<Device>)> {
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

        info!("Checking device /dev/input/{filename}");

        let Ok(dev) = Device::open(ent.path()) else {
            warn!("Unable to open device /dev/input/{filename}, skipping");
            continue;
        };

        if let Some(name) = dev.name() {
            if name == "touchPad" {
                info!("Found touch pad device at /dev/input/{filename}");
                touchpad_dev = Some(dev);
            } else if name == "TitanKey" {
                info!("Found keyboard device at /dev/input/{filename}");
                keyboard_dev = Some(dev);
            }
        }
    }

    Ok((touchpad_dev, keyboard_dev))
}
