`titan2-touchpadd`
---

A daemon to convert [Unihertz Titan 2](https://www.unihertz.com/products/titan-2)'s touchpad input, integrated
with the keyboard, to a mouse pointer input with gestures support. This is implemented via [uinput](https://kernel.org/doc/html/v6.1/input/uinput.html).

Currently, the following gestures are implemented:

- Moving the pointer: tapping and moving a finger along the keyboard
- Left-click: a short single tap
- Right-click: a long single tap
- Drag: double tap, then drag the finger (without releasing the second tap) along the keyboard
- Vertical scrolling: tapping and moving a finger along the left or right edges

In addition, this daemon also implements touch rejection when a keyboard key press is detected.

Building
---

The recommended way of building this is to install [cross](https://github.com/cross-rs/cross) and simply run

```
cross build --target aarch64-unknown-linux-musl --release
```

Your built binary will be ready at `target/aarch64-unknown-linux-musl/release/titan2-touchpadd`. Using `musl` allows
us to avoid installing and importing the entire Android NDK, and allows the resulting binary to work on even non-Android
environments.

Usage
---

You will need to launch this daemon as root or a user that has access to `/dev/input` and `/dev/uinput`, with the
corresponding SELinux permissions (if on Android). You will also need a way to block the original input devices from being
used by the OS. On [PeterGSI](https://gitea.angry.im/PeterGSI), this is done via a [patch](https://gitea.angry.im/PeterGSI/patches/src/branch/aosp16/frameworks/native/0007-inputflinger-Allow-ignoring-touch-devices-using-a-sp.patch) to `inputflinger`.

This daemon is explicitly designed to not actually rely on anything Android-specific, so should there exist a port
of Linux Mobile (like Halium-based distributions such as UBports or Droidian, which should be within the realm of possibility),
it should still work as-is.

Why?
---

The old trick from the OG Titan days of setting

```
touch.deviceType = pointer
```

no longer works since Android 14 switched to the ChromeOS touchpad stack. The new stack 
requires true multitouch, which the Titan 2 does not implement. Even if one gets basic functionalities
working, using this as-is like a trackpad is still suboptimal, since there would be no way of performing, for
example, scrolling, right-clicking, or dragging.

Why not a kernel driver?
---

1. Because Unihertz doesn't open-source their official kernel drivers
2. Implementing this in the kernel would be a huge pain; it might be trivial to fix the exported events so that Android's touchpad stack works, but gesture detection will still not work properly without true multitouch. Touch rejection on keyboard events will also require a lot of custom plumbing. At that point, simply re-exposing a `uinput` device is just easier.
