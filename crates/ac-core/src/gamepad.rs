//! Reads gamepad button presses from Linux evdev, to bind them in AC.
//!
//! Wine/Proton exposes pads to AC as XInput, so buttons are reported by
//! their XInput names (Cross = `A`, Circle = `B`, ...), which is what
//! `controls.ini` stores in `XBOXBUTTON`.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Result, bail};

const BTN_SOUTH: usize = 0x130;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
const O_NONBLOCK: i32 = 0o4000;
const BTN_TL2: u16 = 0x138;
const BTN_TR2: u16 = 0x139;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RZ: u16 = 0x05;

unsafe extern "C" {
    fn ioctl(fd: std::ffi::c_int, request: std::ffi::c_ulong, ...) -> std::ffi::c_int;
}

/// The kernel's `(minimum, maximum)` for an absolute axis (`EVIOCGABS`), if the device has it.
fn abs_range(file: &File, axis: u16) -> Option<(i32, i32)> {
    use std::os::fd::AsRawFd;
    // _IOR('E', 0x40 + axis, struct input_absinfo): value, min, max, fuzz, flat, resolution.
    let request = (2u64 << 30) | (24 << 16) | (0x45 << 8) | (0x40 + axis as u64);
    let mut info = [0i32; 6];
    // SAFETY: `info` is the 24-byte `input_absinfo` this request fills; the fd is open.
    let rc = unsafe { ioctl(file.as_raw_fd(), request as std::ffi::c_ulong, info.as_mut_ptr()) };
    (rc >= 0 && info[2] > info[1]).then_some((info[1], info[2]))
}

/// State for stick/axis normalization and hysteresis.
#[derive(Debug, Clone, Copy)]
struct AxisState {
    min: i32,
    max: i32,
    calibrated: bool,
    neg_engaged: bool,
    pos_engaged: bool,
}

impl Default for AxisState {
    fn default() -> Self {
        Self {
            min: -32768,
            max: 32767,
            calibrated: false,
            neg_engaged: false,
            pos_engaged: false,
        }
    }
}

impl AxisState {
    fn with_range(range: Option<(i32, i32)>) -> Self {
        match range {
            Some((min, max)) => Self { min, max, calibrated: true, ..Self::default() },
            None => Self::default(),
        }
    }

    fn observe(&mut self, val: i32) {
        if !self.calibrated {
            self.calibrated = true;
            // If the initial value is positive and well above 0 (like 128 in 0..255 range),
            // assume 0..255 DS4/unsigned pad range; otherwise assume signed -32768..32767.
            if (64..=192).contains(&val) {
                self.min = 0;
                self.max = 255;
            } else {
                self.min = -32768;
                self.max = 32767;
            }
        }
    }

    /// Process a value update and return newly engaged directions (-1 for negative, +1 for positive).
    fn update(&mut self, val: i32) -> Option<i32> {
        self.observe(val);

        let center = (self.min + self.max) / 2;
        let half_range = ((self.max - self.min).abs() / 2).max(1) as f32;
        let norm = (val - center) as f32 / half_range; // approximately -1.0 .. 1.0

        let mut newly_pressed = None;

        // Negative direction (Left / Up)
        if norm < -0.50 {
            if !self.neg_engaged {
                self.neg_engaged = true;
                newly_pressed = Some(-1);
            }
        } else if norm > -0.30 {
            self.neg_engaged = false;
        }

        // Positive direction (Right / Down)
        if norm > 0.50 {
            if !self.pos_engaged {
                self.pos_engaged = true;
                newly_pressed = Some(1);
            }
        } else if norm < 0.30 {
            self.pos_engaged = false;
        }

        newly_pressed
    }
}

/// State for analog triggers (ABS_Z / ABS_RZ).
#[derive(Debug, Clone, Copy)]
struct TriggerState {
    min: i32,
    max: i32,
    calibrated: bool,
    engaged: bool,
}

impl Default for TriggerState {
    fn default() -> Self {
        Self {
            min: 0,
            max: 255,
            calibrated: false,
            engaged: false,
        }
    }
}

impl TriggerState {
    fn with_range(range: Option<(i32, i32)>) -> Self {
        match range {
            Some((min, max)) => Self { min, max, calibrated: true, ..Self::default() },
            None => Self::default(),
        }
    }

    fn observe(&mut self, val: i32) {
        if !self.calibrated {
            self.calibrated = true;
            // Triggers can be 0..255 (typical) or -32768..32767 on some drivers
            if val < 0 {
                self.min = -32768;
                self.max = 32767;
            } else {
                self.min = 0;
                self.max = 255;
            }
        }
    }

    fn update(&mut self, val: i32) -> bool {
        self.observe(val);
        // Crossing 50% of the axis range engages the trigger
        let threshold = self.min + (self.max - self.min) / 2;
        let is_past = val >= threshold;
        if is_past && !self.engaged {
            self.engaged = true;
            true
        } else {
            if !is_past {
                self.engaged = false;
            }
            false
        }
    }
}

#[derive(Debug)]
struct DeviceState {
    file: File,
    vendor: u16,
    x_axis: AxisState,
    y_axis: AxisState,
    l_trigger: TriggerState,
    r_trigger: TriggerState,
    hat_x: i32,
    hat_y: i32,
    btn_dpad_up: bool,
    btn_dpad_down: bool,
    btn_dpad_left: bool,
    btn_dpad_right: bool,
}

/// Hold-to-repeat tracking for a navigation direction.
#[derive(Debug, Clone, Copy)]
struct RepeatState {
    dir: &'static str,
    press_time: std::time::Instant,
    last_emit_time: std::time::Instant,
}

/// Controller navigation reader.
///
/// Discovers open evdev devices and maps d-pad, left stick, triggers, and buttons
/// into navigation events with hold-to-repeat for directional controls.
///
/// Axis ranges come from the kernel (`EVIOCGABS`); if that fails the first value seen picks
/// 0..255 (DS4-style) or signed 16-bit. Triggers accept BTN_TL2/BTN_TR2 key events and
/// ABS_Z/ABS_RZ crossing 50% of their range.
pub struct Nav {
    devices: Vec<DeviceState>,
    repeat: Option<RepeatState>,
}

impl Nav {
    pub fn open() -> Result<Self> {
        let mut devices = Vec::new();
        let mut denied = None;
        for (path, _, vendor, _) in scan_gamepad_devices() {
            match OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(&path) {
                Ok(file) => {
                    devices.push(DeviceState {
                        vendor,
                        x_axis: AxisState::with_range(abs_range(&file, ABS_X)),
                        y_axis: AxisState::with_range(abs_range(&file, ABS_Y)),
                        l_trigger: TriggerState::with_range(abs_range(&file, ABS_Z)),
                        r_trigger: TriggerState::with_range(abs_range(&file, ABS_RZ)),
                        hat_x: 0,
                        hat_y: 0,
                        btn_dpad_up: false,
                        btn_dpad_down: false,
                        btn_dpad_left: false,
                        btn_dpad_right: false,
                        file,
                    });
                }
                Err(e) => denied = Some(format!("{}: {e}", path.display())),
            }
        }
        if devices.is_empty() {
            match denied {
                Some(e) => bail!("can't read the controller ({e}); add yourself to the input group"),
                None => bail!("no controller found; connect it and try again"),
            }
        }
        Ok(Self {
            devices,
            repeat: None,
        })
    }

    /// True if any opened device has vendor 0x054c (Sony Interactive Entertainment / PlayStation).
    pub fn is_playstation(&self) -> bool {
        self.devices.iter().any(|d| d.vendor == 0x054c)
    }

    /// Returns whether a direction is currently held active on any device.
    fn is_dir_held(&self, dir: &'static str) -> bool {
        self.devices.iter().any(|d| match dir {
            "DPAD_UP" => d.hat_y < 0 || d.btn_dpad_up || d.y_axis.neg_engaged,
            "DPAD_DOWN" => d.hat_y > 0 || d.btn_dpad_down || d.y_axis.pos_engaged,
            "DPAD_LEFT" => d.hat_x < 0 || d.btn_dpad_left || d.x_axis.neg_engaged,
            "DPAD_RIGHT" => d.hat_x > 0 || d.btn_dpad_right || d.x_axis.pos_engaged,
            _ => false,
        })
    }

    /// Polls all open devices and returns ALL semantic presses since the last call.
    ///
    /// For the 4 directions ("DPAD_UP", "DPAD_DOWN", "DPAD_LEFT", "DPAD_RIGHT"),
    /// hold-to-repeat is applied based on `now`:
    /// - first repeat after 350 ms
    /// - then every 90 ms
    /// - speeding to every 45 ms after holding for 1 s.
    ///
    /// Buttons never repeat.
    pub fn poll(&mut self, now: std::time::Instant) -> Vec<&'static str> {
        let mut events = Vec::new();
        let mut buf = [0u8; EVENT_SIZE * 64];

        for d in &mut self.devices {
            while let Ok(n) = d.file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for ev in buf[..n].as_chunks::<EVENT_SIZE>().0 {
                    let t = &ev[EVENT_SIZE - 8..];
                    let kind = u16::from_ne_bytes([t[0], t[1]]);
                    let code = u16::from_ne_bytes([t[2], t[3]]);
                    let value = i32::from_ne_bytes([t[4], t[5], t[6], t[7]]);

                    match (kind, code, value) {
                        (EV_KEY, c, 1) => {
                            match c {
                                BTN_TL2 => events.push("LTRIGGER"),
                                BTN_TR2 => events.push("RTRIGGER"),
                                0x220 => {
                                    d.btn_dpad_up = true;
                                    events.push("DPAD_UP");
                                }
                                0x221 => {
                                    d.btn_dpad_down = true;
                                    events.push("DPAD_DOWN");
                                }
                                0x222 => {
                                    d.btn_dpad_left = true;
                                    events.push("DPAD_LEFT");
                                }
                                0x223 => {
                                    d.btn_dpad_right = true;
                                    events.push("DPAD_RIGHT");
                                }
                                _ => {
                                    if let Some(name) = button_name(c) {
                                        events.push(name);
                                    }
                                }
                            }
                        }
                        (EV_KEY, c, 0) => {
                            match c {
                                0x220 => d.btn_dpad_up = false,
                                0x221 => d.btn_dpad_down = false,
                                0x222 => d.btn_dpad_left = false,
                                0x223 => d.btn_dpad_right = false,
                                _ => {}
                            }
                        }
                        (EV_ABS, ABS_HAT0X, val) => {
                            let old = d.hat_x;
                            d.hat_x = val;
                            if val < 0 && old >= 0 {
                                events.push("DPAD_LEFT");
                            } else if val > 0 && old <= 0 {
                                events.push("DPAD_RIGHT");
                            }
                        }
                        (EV_ABS, ABS_HAT0Y, val) => {
                            let old = d.hat_y;
                            d.hat_y = val;
                            if val < 0 && old >= 0 {
                                events.push("DPAD_UP");
                            } else if val > 0 && old <= 0 {
                                events.push("DPAD_DOWN");
                            }
                        }
                        (EV_ABS, ABS_X, val) => {
                            if let Some(dir) = d.x_axis.update(val) {
                                if dir < 0 {
                                    events.push("DPAD_LEFT");
                                } else {
                                    events.push("DPAD_RIGHT");
                                }
                            }
                        }
                        (EV_ABS, ABS_Y, val) => {
                            if let Some(dir) = d.y_axis.update(val) {
                                if dir < 0 {
                                    events.push("DPAD_UP");
                                } else {
                                    events.push("DPAD_DOWN");
                                }
                            }
                        }
                        (EV_ABS, ABS_Z, val) => {
                            if d.l_trigger.update(val) {
                                events.push("LTRIGGER");
                            }
                        }
                        (EV_ABS, ABS_RZ, val) => {
                            if d.r_trigger.update(val) {
                                events.push("RTRIGGER");
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Check if any new directional press occurred in this poll
        let last_new_dir = events
            .iter()
            .copied()
            .rfind(|e| matches!(*e, "DPAD_UP" | "DPAD_DOWN" | "DPAD_LEFT" | "DPAD_RIGHT"));

        if let Some(dir) = last_new_dir {
            self.repeat = Some(RepeatState {
                dir,
                press_time: now,
                last_emit_time: now,
            });
        } else if let Some(repeat) = self.repeat {
            // Check if still held
            if !self.is_dir_held(repeat.dir) {
                self.repeat = None;
            } else {
                let hold_dur = now.saturating_duration_since(repeat.press_time);
                if hold_dur >= std::time::Duration::from_millis(350) {
                    let interval = if hold_dur >= std::time::Duration::from_millis(1000) {
                        std::time::Duration::from_millis(45)
                    } else {
                        std::time::Duration::from_millis(90)
                    };

                    if now.saturating_duration_since(repeat.last_emit_time) >= interval {
                        events.push(repeat.dir);
                        self.repeat = Some(RepeatState {
                            last_emit_time: now,
                            ..repeat
                        });
                    }
                }
            }
        }

        events
    }
}
/// `struct input_event`: a `timeval` (two longs), then u16 type, u16 code, i32 value.
const EVENT_SIZE: usize = 2 * size_of::<usize>() + 8;

/// Whether a `capabilities/key` bitmap (hex longs, most significant first) has `bit`.
fn has_key(bitmap: &str, bit: usize) -> bool {
    let bits = usize::BITS as usize;
    let words: Vec<&str> = bitmap.split_whitespace().rev().collect();
    words
        .get(bit / bits)
        .and_then(|w| usize::from_str_radix(w, 16).ok())
        .is_some_and(|w| w & (1 << (bit % bits)) != 0)
}

/// `/dev/input/event*` devices that have gamepad face buttons and sticks/d-pad axes.
/// Requiring an absolute axis filters out virtual keyboard/mouse uinput devices like `ydotoold`
/// that advertise all key codes with `capabilities/abs == 0`.
/// Returns (path, label, vendor_id, sys_event_name).
fn scan_gamepad_devices() -> Vec<(PathBuf, String, u16, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir("/sys/class/input").into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") {
            continue;
        }
        let dev = entry.path().join("device");
        let keys = fs::read_to_string(dev.join("capabilities/key")).unwrap_or_default();
        let abs = fs::read_to_string(dev.join("capabilities/abs")).unwrap_or_default();
        let has_pad_axis = has_key(&abs, ABS_X as usize)
            || has_key(&abs, ABS_Y as usize)
            || has_key(&abs, ABS_HAT0X as usize)
            || has_key(&abs, ABS_HAT0Y as usize);
        if has_key(&keys, BTN_SOUTH) && has_pad_axis {
            let label = fs::read_to_string(dev.join("name")).unwrap_or_default().trim().to_owned();
            let vendor_str = fs::read_to_string(dev.join("id/vendor")).unwrap_or_default();
            let vendor = u16::from_str_radix(vendor_str.trim(), 16).unwrap_or(0);
            out.push((PathBuf::from("/dev/input").join(&name), label, vendor, name));
        }
    }
    out
}

/// `/dev/input/event*` devices that have gamepad face buttons.
pub fn devices() -> Vec<(PathBuf, String)> {
    scan_gamepad_devices()
        .into_iter()
        .map(|(path, label, _, _)| (path, label))
        .collect()
}

fn button_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0x130 => "A",
        0x131 => "B",
        0x133 => "Y",
        0x134 => "X",
        0x136 => "LSHOULDER",
        0x137 => "RSHOULDER",
        0x13a => "BACK",
        0x13b => "START",
        0x13d => "LEFT_THUMB",
        0x13e => "RIGHT_THUMB",
        // AC's dpad codes when a driver reports it as buttons.
        0x220 => "DPAD_UP",
        0x221 => "DPAD_DOWN",
        0x222 => "DPAD_LEFT",
        0x223 => "DPAD_RIGHT",
        _ => return None,
    })
}

/// What a press turned into.
pub enum Press {
    Button(&'static str),
    /// A button AC can't bind (triggers are throttle/brake, PS is Steam's).
    Unbindable(u16),
}

pub struct Reader {
    files: Vec<File>,
    pub names: Vec<String>,
}

impl Reader {
    pub fn open() -> Result<Self> {
        let mut files = Vec::new();
        let mut names = Vec::new();
        let mut denied = None;
        for (path, name, _, _) in scan_gamepad_devices() {
            match OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(&path) {
                Ok(f) => {
                    files.push(f);
                    names.push(name);
                }
                Err(e) => denied = Some(format!("{}: {e}", path.display())),
            }
        }
        if files.is_empty() {
            match denied {
                Some(e) => bail!("can't read the controller ({e}); add yourself to the input group"),
                None => bail!("no controller found; connect it and try again"),
            }
        }
        Ok(Self { files, names })
    }

    /// Returns the first new press since the last call, without blocking.
    pub fn poll(&mut self) -> Option<Press> {
        let mut buf = [0u8; EVENT_SIZE * 64];
        for f in &mut self.files {
            while let Ok(n) = f.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for ev in buf[..n].as_chunks::<EVENT_SIZE>().0 {
                    let t = &ev[EVENT_SIZE - 8..];
                    let kind = u16::from_ne_bytes([t[0], t[1]]);
                    let code = u16::from_ne_bytes([t[2], t[3]]);
                    let value = i32::from_ne_bytes([t[4], t[5], t[6], t[7]]);
                    let press = match (kind, code, value) {
                        (EV_KEY, c, 1) => Some(button_name(c).map(Press::Button).unwrap_or(Press::Unbindable(c))),
                        (EV_ABS, ABS_HAT0X, -1) => Some(Press::Button("DPAD_LEFT")),
                        (EV_ABS, ABS_HAT0X, 1) => Some(Press::Button("DPAD_RIGHT")),
                        (EV_ABS, ABS_HAT0Y, -1) => Some(Press::Button("DPAD_UP")),
                        (EV_ABS, ABS_HAT0Y, 1) => Some(Press::Button("DPAD_DOWN")),
                        _ => None,
                    };
                    if press.is_some() {
                        return press;
                    }
                }
            }
        }
        None
    }
}
