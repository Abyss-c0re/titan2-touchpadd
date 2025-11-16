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

In addition, this daemon also implements touch rejection when a keyboard key press is detected. When the env variable
`KEYBOARD_FEATURES` is set to `true`, the following keyboard quality-of-life improvement is also activated:

- Shift (caps), Sym, and Fn keys become "sticky": double-clicking them "locks" them into the pressed state until they are cancelled by another click of the same key or a conflicting key (such as backspace).
- Simulated Left/Up/Right/Down key presses: swiping the finger quickly in the corresponding direction. This also scrolls the content in some apps, but is mainly useful for selecting candidates in some input methods like [fcitx5-android](https://fcitx5-android.github.io/).
- Numeric keys simulation: double-tapping on the top row of keys (but without pressing them!) simulates numeric keys 0-6. Like above, this is mainly useful for input methods.

Building
---

The recommended way of building this is to install [cross](https://github.com/cross-rs/cross) and simply run

```
cross build --target aarch64-unknown-linux-musl --release
```

Your built binary will be ready at `target/aarch64-unknown-linux-musl/release/titan2-touchpadd`. Using `musl` allows
us to avoid installing and importing the entire Android NDK, and allows the resulting binary to work on even non-Android
environments.

(You can also find a prebuilt binary at <https://gitea.angry.im/PeterGSI/android_vendor_prebuilts_titan2-touchpadd>)

Usage
---

You will need to launch this daemon as root or a user that has access to `/dev/input` and `/dev/uinput`, with the
corresponding SELinux permissions (if on Android). The easiest way to do this on stock is to launch the binary using a terminal
program with root access like Termux. This program also tries to grab exclusive access to the touchpad input (and the keyboard,
if `KEYBOARD_FEATURES` is set to `true`) so that it does not conflict with the OS's native gestures.

Here's an example of how to integrate this into an AOSP build:

<https://gitea.angry.im/PeterGSI/android_vendor_prebuilts_titan2-touchpadd>  
<https://gitea.angry.im/PeterGSI/android_device_peter_gsi>

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
