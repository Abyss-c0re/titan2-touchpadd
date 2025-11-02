use std::os::unix::fs::FileTypeExt;

use evdev::Device;
use eyre::eyre;
use tracing::{error, info, warn};

fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::init();

    info!("Detecting Titan 2's touchpad input...");

    let Some(touchpad_dev) = find_touchpad_dev()? else {
        error!("No touchpad device found, exitting");
        return Err(eyre!("No touchpad device found"));
    };

    Ok(())
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
