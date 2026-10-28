//! Kernel input devices (uinput) for a display: a pointer with absolute positions (like a
//! virtual machine's "tablet") and a keyboard. Events from a viewer are injected through
//! them, so the desktop handles them like any mouse and keyboard.

use std::{
    fs::File,
    io::Write,
    os::{fd::AsFd, unix::fs::OpenOptionsExt},
};

use anyhow::{Context, Result};
use rustix::ioctl::{IntegerSetter, NoArg, Opcode, Setter, ioctl, opcode};
use vmon_proto::input::{Button, InputEvent, evdev_key, evdev_keys};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BUS_VIRTUAL: u16 = 0x06;
/// Absolute positions run 0..=ABS_MAX across the display.
const ABS_MAX: i32 = 65535;
/// A wheel notch in high-resolution units.
const NOTCH: i32 = 120;

const UI_DEV_CREATE: Opcode = opcode::none(b'U', 1);
const UI_DEV_DESTROY: Opcode = opcode::none(b'U', 2);
const UI_DEV_SETUP: Opcode = opcode::write::<UinputSetup>(b'U', 3);
const UI_ABS_SETUP: Opcode = opcode::write::<UinputAbsSetup>(b'U', 4);
const UI_SET_EVBIT: Opcode = opcode::write::<i32>(b'U', 100);
const UI_SET_KEYBIT: Opcode = opcode::write::<i32>(b'U', 101);
const UI_SET_RELBIT: Opcode = opcode::write::<i32>(b'U', 102);
const UI_SET_ABSBIT: Opcode = opcode::write::<i32>(b'U', 103);

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: AbsInfo,
}

/// One uinput device; destroyed when dropped.
struct Device {
    file: File,
    name: String,
}

impl Device {
    /// `bits`: (event type, codes) the device sends; `abs`: absolute axes with their range.
    fn create(name: &str, product: u16, bits: &[(u16, Vec<u16>)], abs: &[u16]) -> Result<Device> {
        let file = File::options()
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open("/dev/uinput")
            .context("opening /dev/uinput (is the uinput module loaded?)")?;
        // SAFETY: each opcode gets the argument the uinput driver expects: the `UI_SET_*BIT`
        // ones an integer by value, the setups a pointer to their struct.
        unsafe {
            set_bit::<UI_SET_EVBIT>(&file, EV_SYN)?;
            for (ty, codes) in bits {
                set_bit::<UI_SET_EVBIT>(&file, *ty)?;
                for &code in codes {
                    match *ty {
                        EV_KEY => set_bit::<UI_SET_KEYBIT>(&file, code)?,
                        EV_REL => set_bit::<UI_SET_RELBIT>(&file, code)?,
                        EV_ABS => set_bit::<UI_SET_ABSBIT>(&file, code)?,
                        _ => {}
                    }
                }
            }
            for &code in abs {
                let setup = UinputAbsSetup {
                    code,
                    absinfo: AbsInfo { value: 0, minimum: 0, maximum: ABS_MAX, fuzz: 0, flat: 0, resolution: 0 },
                };
                ioctl(&file, Setter::<UI_ABS_SETUP, UinputAbsSetup>::new(setup))?;
            }
            let mut setup = UinputSetup {
                id: InputId { bustype: BUS_VIRTUAL, vendor: 0x1209, product, version: 1 },
                name: [0; 80],
                ff_effects_max: 0,
            };
            let bytes = name.as_bytes();
            setup.name[..bytes.len().min(79)].copy_from_slice(&bytes[..bytes.len().min(79)]);
            ioctl(&file, Setter::<UI_DEV_SETUP, UinputSetup>::new(setup))?;
            ioctl(&file, NoArg::<UI_DEV_CREATE>::new())?;
        }
        Ok(Device { file, name: name.into() })
    }

    /// Writes events followed by a sync (one report).
    fn send(&mut self, events: &[(u16, u16, i32)]) -> Result<()> {
        let mut buffer = Vec::with_capacity((events.len() + 1) * 24);
        for &(ty, code, value) in events.iter().chain(&[(EV_SYN, SYN_REPORT, 0)]) {
            // struct input_event on 64-bit Linux: a zero timeval (the kernel stamps it), then
            // type, code, value.
            buffer.extend_from_slice(&[0; 16]);
            buffer.extend_from_slice(&ty.to_ne_bytes());
            buffer.extend_from_slice(&code.to_ne_bytes());
            buffer.extend_from_slice(&value.to_ne_bytes());
        }
        self.file.write_all(&buffer).context("writing input events")
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: UI_DEV_DESTROY takes no argument.
        let _ = unsafe { ioctl(&self.file, NoArg::<UI_DEV_DESTROY>::new()) };
    }
}

/// # Safety
/// `OP` must be one of uinput's `UI_SET_*BIT` opcodes.
unsafe fn set_bit<const OP: Opcode>(file: &impl AsFd, bit: u16) -> Result<()> {
    // SAFETY: the caller passes a `UI_SET_*BIT` opcode, which takes the bit by value.
    unsafe { ioctl(file, IntegerSetter::<OP>::new_usize(bit as usize))? };
    Ok(())
}

/// A display's pointer and keyboard.
pub struct Inputs {
    pointer: Device,
    keyboard: Device,
    /// High-resolution wheel movement not yet a whole notch (x, y).
    wheel: (i32, i32),
}

impl Inputs {
    pub fn create(display: &str) -> Result<Inputs> {
        let pointer = Device::create(
            &format!("{display} pointer"),
            1,
            &[
                (EV_KEY, vec![BTN_LEFT, BTN_RIGHT, BTN_MIDDLE]),
                (EV_REL, vec![REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES]),
                (EV_ABS, vec![ABS_X, ABS_Y]),
            ],
            &[ABS_X, ABS_Y],
        )?;
        let keyboard = Device::create(&format!("{display} keyboard"), 2, &[(EV_KEY, evdev_keys().collect())], &[])?;
        Ok(Inputs { pointer, keyboard, wheel: (0, 0) })
    }

    pub fn pointer_name(&self) -> &str {
        &self.pointer.name
    }

    pub fn send(&mut self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::Move { x, y } => {
                let scale = |v: f32| (v.clamp(0.0, 1.0) * ABS_MAX as f32).round() as i32;
                self.pointer.send(&[(EV_ABS, ABS_X, scale(*x)), (EV_ABS, ABS_Y, scale(*y))])
            }
            InputEvent::Button { button, down } => {
                let code = match button {
                    Button::Left => BTN_LEFT,
                    Button::Right => BTN_RIGHT,
                    Button::Middle => BTN_MIDDLE,
                };
                self.pointer.send(&[(EV_KEY, code, *down as i32)])
            }
            InputEvent::Wheel { dx, dy } => {
                let events = wheel_events(&mut self.wheel, *dx, *dy);
                self.pointer.send(&events)
            }
            InputEvent::Key { code, down } => match evdev_key(code) {
                Some(key) => self.keyboard.send(&[(EV_KEY, key, *down as i32)]),
                None => Ok(()),
            },
        }
    }
}

/// The wheel events for a scroll of (`dx`, `dy`) high-resolution units: the high-resolution
/// axes always, the notch axes once a whole notch has built up. Positive `dy` scrolls down,
/// which is a negative REL_WHEEL.
fn wheel_events(rest: &mut (i32, i32), dx: i32, dy: i32) -> Vec<(u16, u16, i32)> {
    let mut events = Vec::new();
    if dy != 0 {
        events.push((EV_REL, REL_WHEEL_HI_RES, -dy));
        rest.1 += dy;
        let notches = rest.1 / NOTCH;
        if notches != 0 {
            events.push((EV_REL, REL_WHEEL, -notches));
            rest.1 -= notches * NOTCH;
        }
    }
    if dx != 0 {
        events.push((EV_REL, REL_HWHEEL_HI_RES, dx));
        rest.0 += dx;
        let notches = rest.0 / NOTCH;
        if notches != 0 {
            events.push((EV_REL, REL_HWHEEL, notches));
            rest.0 -= notches * NOTCH;
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_struct_sizes() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<UinputAbsSetup>(), 28);
    }

    #[test]
    fn wheel_notches_build_up() {
        let mut rest = (0, 0);
        assert_eq!(wheel_events(&mut rest, 0, 60), vec![(EV_REL, REL_WHEEL_HI_RES, -60)]);
        assert_eq!(wheel_events(&mut rest, 0, 60), vec![(EV_REL, REL_WHEEL_HI_RES, -60), (EV_REL, REL_WHEEL, -1)]);
        assert_eq!(rest, (0, 0));
        assert_eq!(
            wheel_events(&mut rest, 240, -360),
            vec![
                (EV_REL, REL_WHEEL_HI_RES, 360),
                (EV_REL, REL_WHEEL, 3),
                (EV_REL, REL_HWHEEL_HI_RES, 240),
                (EV_REL, REL_HWHEEL, 2)
            ]
        );
    }
}
