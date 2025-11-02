use std::os::unix::fs::FileTypeExt;

use evdev::{AttributeSet, Device, KeyCode, RelativeAxisCode, uinput};
use eyre::{OptionExt, eyre};
use tracing::{error, info, warn};

mod evloop;

fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();

    info!("Detecting Titan 2's touchpad input...");

    let Some(touchpad_dev) = find_touchpad_dev()? else {
        error!("No touchpad device found, exitting");
        return Err(eyre!("No touchpad device found"));
    };

    info!("Creating virtual mouse input...");
    let uinput_axes = {
        let mut axes = AttributeSet::new();
        axes.insert(RelativeAxisCode::REL_X);
        axes.insert(RelativeAxisCode::REL_Y);
        axes.insert(RelativeAxisCode::REL_WHEEL);
        axes.insert(RelativeAxisCode::REL_HWHEEL);
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

    evloop::run_evloop(touchpad_dev, uinput_dev)
}

fn find_touchpad_dev() -> eyre::Result<Option<Device>> {
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

        if let Some(name) = dev.name()
            && name == "touchPad"
        {
            info!("Found touch pad device at /dev/input/{filename}");
            return Ok(Some(dev));
        }
    }

    Ok(None)
}
