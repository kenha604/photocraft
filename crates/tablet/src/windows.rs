//! Win32 window subclass for pen data. Like `macos.rs`, this module allows `unsafe`: the Win32
//! calls that install the subclass and read a pointer's pen info (each has a `SAFETY:` comment).
//! Everything it reads goes through the pure, tested mapping in [`crate::wm_pointer`].
//!
//! winit turns `WM_POINTER` pen input into touch events with a force and drops the tilt,
//! rotation and eraser flag of `POINTER_PEN_INFO`. The subclass sees each message of winit's
//! window before winit does, on the window's thread, so the sample is current when the UI handles
//! the matching pointer event. Every message is passed on unchanged.

#![allow(unsafe_code)]

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::Pointer::{GetPointerPenInfo, GetPointerType, POINTER_PEN_INFO};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageExtraInfo, GetWindowThreadProcessId, PT_PEN, WM_NCDESTROY};

use crate::wm_pointer::{RawEvent, State, msg};
use crate::{Error, Sample, Update, deliver};

/// Identifies this subclass on the window (with the procedure's address).
const SUBCLASS_ID: usize = 0x5043_5450;

struct Shared {
    state: RefCell<State>,
    callback: Box<dyn Fn(Option<Sample>)>,
}

/// Keeps the subclass installed; dropping it removes the subclass. Window-thread only (not
/// `Send`).
pub struct Monitor {
    hwnd: HWND,
    shared: *mut Shared,
}

impl Monitor {
    /// Subclass the window with the raw handle `hwnd`. `callback` gets `Some(sample)` for pen
    /// messages and `None` when a mouse moves or the pen leaves; it runs on the window's thread.
    /// Call on the thread that created the window.
    pub fn install(hwnd: isize, callback: impl Fn(Option<Sample>) + 'static) -> Result<Self, Error> {
        let hwnd = hwnd as HWND;
        // SAFETY: only reads; the process id out-pointer may be null, and a handle that is not a
        // window returns 0.
        let owner = unsafe { GetWindowThreadProcessId(hwnd, std::ptr::null_mut()) };
        if owner == 0 {
            return Err(Error::Platform("not a window".into()));
        }
        // SAFETY: no arguments, cannot fail.
        if owner != unsafe { GetCurrentThreadId() } {
            return Err(Error::NotMainThread);
        }
        let shared = Box::into_raw(Box::new(Shared { state: RefCell::new(State::default()), callback: Box::new(callback) }));
        // SAFETY: `hwnd` is a window of this thread (checked above), as subclassing requires.
        // `shared` stays allocated until `drop` has removed the subclass, so `subclass_proc`
        // never sees it dangling.
        if unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, shared as usize) } == 0 {
            // SAFETY: the pointer came from `Box::into_raw` above and was not handed out.
            drop(unsafe { Box::from_raw(shared) });
            return Err(Error::Platform("SetWindowSubclass failed".into()));
        }
        Ok(Self { hwnd, shared })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: removes exactly the subclass `install` added; a window that is already gone
        // (it removed the subclass itself on WM_NCDESTROY) just makes the call fail. `Monitor`
        // is not `Send`, so this runs on the window's thread and no `subclass_proc` call is in
        // progress; after it none can start, so the box is ours again.
        unsafe {
            RemoveWindowSubclass(self.hwnd, Some(subclass_proc), SUBCLASS_ID);
            drop(Box::from_raw(self.shared));
        }
    }
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, data: usize) -> LRESULT {
    // SAFETY: `data` is the `Shared` that `install` leaked for this subclass; it outlives the
    // subclass (see `Monitor::drop`). Only shared references are made, on the window's thread.
    let shared = unsafe { &*(data as *const Shared) };
    // A callback that pumps messages can re-enter; `try_borrow_mut` skips instead of panicking.
    if let Some(raw) = read(message, wparam)
        && let Ok(mut st) = shared.state.try_borrow_mut()
        && let Update::Set(sample) = st.handle(&raw)
    {
        deliver(&*shared.callback, sample);
    }
    if message == WM_NCDESTROY {
        // SAFETY: the window is being destroyed; a subclass must be removed before it is gone.
        unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
    }
    // SAFETY: passes the message on unchanged, as a subclass procedure must.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

/// Read the pen fields of a message; `None` for messages the mapping doesn't use and for
/// pointers that are not pens.
fn read(message: u32, wparam: WPARAM) -> Option<RawEvent> {
    match message {
        msg::MOUSE_FIRST..=msg::MOUSE_LAST => {
            // SAFETY: no arguments; reads the extra info of the message being processed.
            let extra = unsafe { GetMessageExtraInfo() };
            Some(RawEvent::Mouse { wparam, extra: extra as usize })
        }
        msg::POINTER_UPDATE | msg::POINTER_DOWN | msg::POINTER_UP | msg::POINTER_LEAVE => {
            // GET_POINTERID_WPARAM: the low word.
            let id = (wparam & 0xFFFF) as u32;
            let mut kind = 0;
            // SAFETY: `kind` is a valid out-pointer for the call.
            if unsafe { GetPointerType(id, &mut kind) } == 0 || kind != PT_PEN {
                return None;
            }
            if message == msg::POINTER_LEAVE {
                return Some(RawEvent::PenLeave);
            }
            // SAFETY: `POINTER_PEN_INFO` is plain data for which all-zero is a valid value, and
            // `info` is a valid out-pointer for the call.
            let mut info: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
            if unsafe { GetPointerPenInfo(id, &mut info) } == 0 {
                return None;
            }
            Some(RawEvent::Pen {
                msg: message,
                pointer_flags: info.pointerInfo.pointerFlags,
                pen_flags: info.penFlags,
                pen_mask: info.penMask,
                pressure: info.pressure,
                rotation: info.rotation,
                tilt_x: info.tiltX,
                tilt_y: info.tiltY,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_on_something_that_is_no_window_is_an_error() {
        assert!(matches!(Monitor::install(0, |_| {}).err(), Some(Error::Platform(_))));
    }

    #[test]
    fn messages_the_mapping_does_not_use_are_not_read() {
        // WM_KEYDOWN, WM_MOUSEWHEEL, WM_NCDESTROY.
        for m in [0x0100, 0x020A, WM_NCDESTROY] {
            assert_eq!(read(m, 0), None, "{m:#x}");
        }
        // A pointer id that names no pointer.
        assert_eq!(read(msg::POINTER_UPDATE, 0xFFFF), None);
    }
}
