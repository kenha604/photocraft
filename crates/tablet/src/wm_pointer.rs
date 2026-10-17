//! Win32 `WM_POINTER` pen fields → [`Sample`]s. Pure (no Win32 calls), so it is tested on every
//! platform; `windows.rs` reads the fields and feeds them here.
//!
//! - `WM_POINTERDOWN` / `WM_POINTERUPDATE` of a pen pointer carry a `POINTER_PEN_INFO`:
//!   `pressure` (0..1024), `tiltX` / `tiltY` (degrees, -90..90, already in the W3C convention)
//!   and `rotation` (degrees, 0..359), each valid only when its `penMask` bit is set. A pen that
//!   is not in contact hovers: pressure 0. `WM_POINTERUP` reports pressure 0 and keeps the
//!   sample instead: it reaches the UI in the same frame as the stroke's last moves, which must
//!   keep the pressure they were drawn with.
//! - `WM_POINTERLEAVE` of a pen pointer: the pen left detection range (or the window).
//! - Mouse messages that Windows did not synthesise from a pen or touch come from a real mouse:
//!   back to `None`. A pen left lying on the tablet stays in range and keeps reporting hover
//!   moves, which must not take over a mouse drag.

use crate::{Sample, Update};

/// Window message ids (winuser.h).
pub mod msg {
    /// `WM_MOUSEMOVE`: the first of the mouse move / button messages.
    pub const MOUSE_FIRST: u32 = 0x0200;
    /// `WM_MBUTTONDBLCLK`: the last of them before the wheel and X-button messages.
    pub const MOUSE_LAST: u32 = 0x0209;
    pub const POINTER_UPDATE: u32 = 0x0245;
    pub const POINTER_DOWN: u32 = 0x0246;
    pub const POINTER_UP: u32 = 0x0247;
    pub const POINTER_LEAVE: u32 = 0x024A;
}

/// `POINTER_FLAG_INCONTACT`: the pen touches the surface.
pub const POINTER_FLAG_INCONTACT: u32 = 0x0004;

/// `PEN_FLAG_*`: the eraser end hovers (`INVERTED`) or touches (`ERASER`).
pub mod pen_flag {
    pub const INVERTED: u32 = 0x0002;
    pub const ERASER: u32 = 0x0004;
}

/// `PEN_MASK_*`: which `POINTER_PEN_INFO` fields the device reports.
pub mod pen_mask {
    pub const PRESSURE: u32 = 0x0001;
    pub const ROTATION: u32 = 0x0002;
    pub const TILT_X: u32 = 0x0004;
    pub const TILT_Y: u32 = 0x0008;
}

/// `MK_LBUTTON | MK_RBUTTON | MK_MBUTTON` in a mouse message's `wParam`.
pub const MK_BUTTONS: usize = 0x0001 | 0x0002 | 0x0010;

/// `GetMessageExtraInfo()` of a mouse message synthesised from a pen or touch has this signature
/// in its upper 24 bits.
pub const EXTRA_SIGNATURE: usize = 0xFF51_5700;
pub const EXTRA_SIGNATURE_MASK: usize = 0xFFFF_FF00;

/// `POINTER_PEN_INFO::pressure` at full pressure.
pub const PRESSURE_MAX: f32 = 1024.0;

/// The pen-related fields of one window message, read as plain values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawEvent {
    /// `WM_POINTERDOWN` / `UPDATE` / `UP` of a pen pointer, with its `POINTER_PEN_INFO`.
    Pen { msg: u32, pointer_flags: u32, pen_flags: u32, pen_mask: u32, pressure: u32, rotation: u32, tilt_x: i32, tilt_y: i32 },
    /// `WM_POINTERLEAVE` of a pen pointer.
    PenLeave,
    /// A mouse move or button message: its `wParam` and `GetMessageExtraInfo()`.
    Mouse { wparam: usize, extra: usize },
}

/// Pen state across messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// A real mouse has a button down: hover moves of a pen lying in range are ignored.
    pub mouse_drag: bool,
}

impl State {
    /// Map one message.
    pub fn handle(&mut self, e: &RawEvent) -> Update {
        match *e {
            RawEvent::Pen { msg, pointer_flags, pen_flags, pen_mask, pressure, rotation, tilt_x, tilt_y } => {
                // Lifting the tip reports pressure 0 (the next hover move says so too).
                if msg == msg::POINTER_UP {
                    return Update::Keep;
                }
                let contact = pointer_flags & POINTER_FLAG_INCONTACT != 0;
                if contact {
                    self.mouse_drag = false;
                } else if self.mouse_drag {
                    return Update::Keep;
                }
                let has = |bit: u32| pen_mask & bit != 0;
                let pressure = match (contact, has(pen_mask::PRESSURE)) {
                    (false, _) => 0.0,
                    (true, true) => pressure as f32 / PRESSURE_MAX,
                    // A pen without a pressure sensor paints like a mouse.
                    (true, false) => 1.0,
                };
                Update::Set(Some(
                    Sample {
                        pressure,
                        tilt_x: if has(pen_mask::TILT_X) { tilt_x as f32 } else { 0.0 },
                        tilt_y: if has(pen_mask::TILT_Y) { tilt_y as f32 } else { 0.0 },
                        rotation: if has(pen_mask::ROTATION) { rotation as f32 } else { 0.0 },
                        eraser: pen_flags & (pen_flag::INVERTED | pen_flag::ERASER) != 0,
                    }
                    .sanitized(),
                ))
            }
            RawEvent::PenLeave => Update::Set(None),
            RawEvent::Mouse { wparam, extra } => {
                if extra & EXTRA_SIGNATURE_MASK == EXTRA_SIGNATURE {
                    return Update::Keep;
                }
                self.mouse_drag = wparam & MK_BUTTONS != 0;
                Update::Set(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: u32 = pen_mask::PRESSURE | pen_mask::ROTATION | pen_mask::TILT_X | pen_mask::TILT_Y;

    fn pen(msg: u32, contact: bool, pressure: u32, tilt: (i32, i32), rotation: u32) -> RawEvent {
        let pointer_flags = if contact { POINTER_FLAG_INCONTACT } else { 0 };
        RawEvent::Pen { msg, pointer_flags, pen_flags: 0, pen_mask: ALL, pressure, rotation, tilt_x: tilt.0, tilt_y: tilt.1 }
    }

    fn set(u: Update) -> Option<Sample> {
        match u {
            Update::Set(s) => s,
            Update::Keep => panic!("expected Set, got Keep"),
        }
    }

    #[test]
    fn pen_messages_carry_pressure_tilt_and_rotation() {
        let mut st = State::default();
        let s = set(st.handle(&pen(msg::POINTER_DOWN, true, 512, (30, -15), 45))).unwrap();
        assert_eq!(s, Sample { pressure: 0.5, tilt_x: 30.0, tilt_y: -15.0, rotation: 45.0, eraser: false });
        let s = set(st.handle(&pen(msg::POINTER_UPDATE, true, 1024, (-60, 60), 0))).unwrap();
        assert_eq!((s.pressure, s.tilt_x, s.tilt_y), (1.0, -60.0, 60.0));
        // Hovering keeps the tilt but has no pressure, whatever the device says.
        let s = set(st.handle(&pen(msg::POINTER_UPDATE, false, 300, (20, 10), 0))).unwrap();
        assert_eq!((s.pressure, s.tilt_x, s.tilt_y), (0.0, 20.0, 10.0));
    }

    #[test]
    fn lifting_the_pen_keeps_the_strokes_pressure() {
        let mut st = State::default();
        assert!(set(st.handle(&pen(msg::POINTER_UPDATE, true, 700, (0, 0), 0))).is_some());
        assert_eq!(st.handle(&pen(msg::POINTER_UP, false, 0, (0, 0), 0)), Update::Keep);
        assert_eq!(st.handle(&RawEvent::PenLeave), Update::Set(None));
    }

    #[test]
    fn fields_the_device_does_not_report_are_ignored() {
        let mut st = State::default();
        let e = RawEvent::Pen { msg: msg::POINTER_UPDATE, pointer_flags: POINTER_FLAG_INCONTACT, pen_flags: 0, pen_mask: 0, pressure: 9, rotation: 77, tilt_x: 40, tilt_y: 40 };
        assert_eq!(set(st.handle(&e)), Some(Sample::default()), "no pressure sensor paints like a mouse");
        let e = RawEvent::Pen { msg: msg::POINTER_UPDATE, pointer_flags: POINTER_FLAG_INCONTACT, pen_flags: 0, pen_mask: ALL, pressure: 9000, rotation: 725, tilt_x: 400, tilt_y: -400 };
        assert_eq!(set(st.handle(&e)), Some(Sample { pressure: 1.0, tilt_x: 90.0, tilt_y: -90.0, rotation: 5.0, eraser: false }));
    }

    #[test]
    fn the_eraser_end_is_reported_hovering_and_touching() {
        let mut st = State::default();
        let end = |pen_flags, contact| RawEvent::Pen {
            msg: msg::POINTER_UPDATE,
            pointer_flags: if contact { POINTER_FLAG_INCONTACT } else { 0 },
            pen_flags,
            pen_mask: ALL,
            pressure: 400,
            rotation: 0,
            tilt_x: 0,
            tilt_y: 0,
        };
        assert!(set(st.handle(&end(pen_flag::INVERTED, false))).unwrap().eraser);
        assert!(set(st.handle(&end(pen_flag::ERASER, true))).unwrap().eraser);
        // The barrel button (PEN_FLAG_BARREL = 1) is not the eraser.
        assert!(!set(st.handle(&end(0x0001, true))).unwrap().eraser);
    }

    #[test]
    fn a_real_mouse_is_a_mouse_and_pen_made_mouse_messages_are_not() {
        let mut st = State::default();
        assert!(set(st.handle(&pen(msg::POINTER_UPDATE, false, 0, (10, 0), 0))).is_some());
        assert_eq!(st.handle(&RawEvent::Mouse { wparam: 0, extra: EXTRA_SIGNATURE | 0x12 }), Update::Keep, "synthesised from the pen");
        assert_eq!(st.handle(&RawEvent::Mouse { wparam: 0, extra: EXTRA_SIGNATURE | 0x80 }), Update::Keep, "synthesised from touch");
        assert_eq!(st.handle(&RawEvent::Mouse { wparam: 0, extra: 0 }), Update::Set(None));
    }

    /// A pen lying on the tablet keeps sending hover moves.
    #[test]
    fn a_resting_pen_does_not_take_over_a_mouse_drag() {
        let mut st = State::default();
        assert_eq!(st.handle(&RawEvent::Mouse { wparam: 0x0001, extra: 0 }), Update::Set(None));
        assert_eq!(st.handle(&pen(msg::POINTER_UPDATE, false, 0, (5, 5), 0)), Update::Keep);
        // Touching down with the pen is deliberate, and its hover counts again afterwards.
        assert!(set(st.handle(&pen(msg::POINTER_DOWN, true, 300, (5, 5), 0))).is_some());
        assert!(set(st.handle(&pen(msg::POINTER_UPDATE, false, 0, (5, 5), 0))).is_some());
        // Releasing the mouse button lets the hover through as well.
        st.handle(&RawEvent::Mouse { wparam: 0x0001, extra: 0 });
        st.handle(&RawEvent::Mouse { wparam: 0, extra: 0 });
        assert!(set(st.handle(&pen(msg::POINTER_UPDATE, false, 0, (5, 5), 0))).is_some());
    }
}
