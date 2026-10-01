use std::os::fd::AsRawFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime};

use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode, SynchronizationCode};
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug)]
pub(crate) struct TouchState {
    pub x: i32,
    pub y: i32,
    pub down: bool,
    pub timestamp: SystemTime,
}

pub(crate) struct TouchStateTracker {
    inner: Device,
    want_grab: Arc<AtomicBool>,
    grabbed: bool,
    blocking_set: bool,
    last_state: TouchState,
    pending_events: Vec<EventSummary>,
}

impl TouchStateTracker {
    pub(crate) fn new(inner: Device, want_grab: Arc<AtomicBool>) -> Self {
        // HID exclusive start while the finger is already down: BTN_TOUCH is
        // edge-triggered. Seed from EVIOCGKEY / EVIOCGABS so the first SYN
        // emits instead of waiting for lift + new down.
        let mut last_state = TouchState {
            x: 0,
            y: 0,
            down: false,
            timestamp: SystemTime::now(),
        };
        if let Ok(keys) = inner.get_key_state() {
            last_state.down = keys.contains(KeyCode::BTN_TOUCH)
                || keys.contains(KeyCode::BTN_TOOL_FINGER);
        }
        if let Ok(abs) = inner.get_absinfo() {
            let mut mx = None;
            let mut my = None;
            let mut sx = None;
            let mut sy = None;
            for (code, info) in abs {
                if code == AbsoluteAxisCode::ABS_MT_POSITION_X {
                    mx = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                    my = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_X {
                    sx = Some(info.value());
                } else if code == AbsoluteAxisCode::ABS_Y {
                    sy = Some(info.value());
                }
            }
            last_state.x = mx.or(sx).unwrap_or(0);
            last_state.y = my.or(sy).unwrap_or(0);
        }
        if last_state.down {
            info!(
                x = last_state.x,
                y = last_state.y,
                "seed live contact (HID attach mid-touch)"
            );
        }
        Self {
            inner,
            want_grab,
            grabbed: false,
            blocking_set: false,
            last_state,
            pending_events: vec![],
        }
    }

    fn apply_grab(&mut self) {
        let want = self.want_grab.load(Ordering::Relaxed);
        if want == self.grabbed {
            return;
        }
        if want {
            if self.inner.grab().is_ok() {
                self.grabbed = true;
                info!("touchPad grab (mouse / HID session)");
            }
        } else if self.inner.ungrab().is_ok() {
            self.grabbed = false;
            info!("touchPad ungrab (trackpad / phone off)");
        }
    }

    fn try_construct_touch_state(&mut self) -> eyre::Result<TouchState> {
        let mut ret = self.last_state.clone();
        let mut seen_x = false;
        let mut seen_y = false;
        let mut seen_btn = false;

        for ev in self.pending_events.iter() {
            match ev {
                EventSummary::AbsoluteAxis(ev, code, val) => {
                    if *code == AbsoluteAxisCode::ABS_MT_POSITION_X {
                        ret.x = *val;
                        seen_x = true;
                    } else if *code == AbsoluteAxisCode::ABS_MT_POSITION_Y {
                        ret.y = *val;
                        seen_y = true;
                    } else if *code == AbsoluteAxisCode::ABS_MT_TRACKING_ID {
                        ret.down = *val >= 0;
                        seen_btn = true;
                    }
                    ret.timestamp = ev.timestamp();
                }
                EventSummary::Key(ev, code, state) => {
                    // Technically the touchpad emits both BTN_TOUCH and BTN_TOOL_FINGER, but we only
                    // use one here.
                    if *code == KeyCode::BTN_TOUCH || *code == KeyCode::BTN_TOOL_FINGER {
                        ret.down = *state == 1;
                    }
                    ret.timestamp = ev.timestamp();
                    seen_btn = true;
                }
                _ => warn!("Ignoring unknown event {:?}", ev),
            }
        }

        if !(seen_btn || seen_x || seen_y) {
            Err(eyre::eyre!("Missing ABS_MT_ position events or BTN_TOUCH"))
        } else {
            self.last_state = ret.clone();
            Ok(ret)
        }
    }
}

impl Iterator for TouchStateTracker {
    type Item = eyre::Result<TouchState>;

    fn next(&mut self) -> Option<Self::Item> {
        // 2.250 heat: idle must block in poll with a real timeout — never a
        // zero-timeout read/ioctl spin when the pad is quiet.
        // fetch_events blocks only if the fd is blocking; enforce that once.
        if !self.blocking_set {
            let _ = self.inner.set_nonblocking(false);
            self.blocking_set = true;
        }
        loop {
            self.apply_grab();
            // Wait up to 500ms for input; park the core while quiet.
            if !poll_fd_readable(self.inner.as_raw_fd(), 500) {
                continue;
            }
            let Ok(events) = self.inner.fetch_events().map(|ev| ev.collect::<Vec<_>>()) else {
                error!("Failed to fetch more events, terminating");
                return None;
            };
            if events.is_empty() {
                // Spurious wake — do not tight-loop
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }

            for cur_event in events {
                let cur_event = cur_event.destructure();

                if let EventSummary::Synchronization(_, syn_code, _) = cur_event {
                    // This is what Titan 2's touchpad uses
                    if syn_code == SynchronizationCode::SYN_REPORT {
                        let ret = self.try_construct_touch_state();
                        debug!("Constructed touch state {ret:?}");
                        self.pending_events.clear();
                        return Some(ret);
                    }

                    // If we don't know about a synchronization event we still clear the pending events
                    self.pending_events.clear();
                } else {
                    self.pending_events.push(cur_event);
                }
            }
        }
    }
}

/// libc poll(2) wrapper — true if fd is readable (or on error to allow fetch to report).
fn poll_fd_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    const POLLIN: i16 = 0x0001;
    let mut pfd = PollFd {
        fd: fd as i32,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: single pollfd, valid fd from Device.
    let rc = unsafe { libc_poll(&mut pfd as *mut PollFd as *mut _, 1, timeout_ms) };
    rc != 0
}

unsafe extern "C" {
    #[link_name = "poll"]
    fn libc_poll(fds: *mut core::ffi::c_void, nfds: libc_nfds_t, timeout: i32) -> i32;
}
type libc_nfds_t = usize;

