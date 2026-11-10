//! Direct Windows Ink pen sampling.
//!
//! winit already forwards Windows Ink pressure as touch force, but that path
//! only covers pointers in contact and can be lost between winit and egui. This
//! module subclasses the primary window and reads `WM_POINTER` frames with
//! `GetPointerPenInfo`, so hover detection and pressure reach the editor even
//! when no touch event is translated.
//!
//! Pen input is an augmentation, not a replacement: a mouse or a touchscreen
//! keeps using the normal pointer/touch path, and non-Windows builds compile
//! this module to no-ops.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use bevy::prelude::*;

/// A native pen sample older than this no longer keeps the brush "in contact".
/// Windows stops sending pointer updates while the pen is stationary, so this
/// must stay comfortably longer than a frame.
const STALE_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
struct PenState {
    /// True once any pen pointer has been seen, including hover.
    detected: bool,
    contact: bool,
    pressure: f32,
    stamp: Option<Instant>,
}

impl PenState {
    const fn new() -> Self {
        Self {
            detected: false,
            contact: false,
            pressure: 0.0,
            stamp: None,
        }
    }
}

static PEN: Mutex<PenState> = Mutex::new(PenState::new());

/// Latest native pen sample as `(contact, pressure)`, or `None` when no pen has
/// ever been seen. A stale sample reports "not in contact".
pub(crate) fn sample() -> Option<(bool, f32)> {
    let state = PEN.lock().ok()?;
    sample_from(*state, Instant::now())
}

fn sample_from(state: PenState, now: Instant) -> Option<(bool, f32)> {
    if !state.detected {
        return None;
    }
    let fresh = state
        .stamp
        .is_some_and(|stamp| now.saturating_duration_since(stamp) < STALE_AFTER);
    if fresh {
        Some((state.contact, state.pressure))
    } else {
        Some((false, 0.0))
    }
}

fn store(contact: bool, pressure: f32) {
    if let Ok(mut state) = PEN.lock() {
        if !state.detected {
            bevy::log::info!("pen input: Windows Ink pen detected");
        }
        if contact && !state.contact {
            bevy::log::info!("pen input: contact at {:.0}% pressure", pressure * 100.0);
        }
        state.detected = true;
        state.contact = contact;
        state.pressure = pressure.clamp(0.0, 1.0);
        state.stamp = Some(Instant::now());
    }
}

/// Install the pointer hook on the primary window. Runs on the main thread
/// because winit windows live there.
pub fn install_pen_input(
    _main_thread: NonSend<crate::app::MainThreadOnly>,
    primary: Query<Entity, With<bevy::window::PrimaryWindow>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let Ok(entity) = primary.single() else {
        return;
    };
    let installed = bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
        let Some(window) = windows.get_window(entity) else {
            return false;
        };
        install_for_window(window)
    });
    if installed {
        bevy::log::info!("pen input: WM_POINTER hook installed");
    } else {
        bevy::log::warn!("pen input: could not install the WM_POINTER hook");
    }
    if installed {
        *done = true;
    }
}

#[cfg(windows)]
fn install_for_window(window: &winit::window::Window) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::SetWindowSubclass;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return false;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    unsafe { SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0).as_bool() }
}

#[cfg(not(windows))]
fn install_for_window(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(windows)]
const SUBCLASS_ID: usize = 0x4E53_4850; // "NSHP"

#[cfg(windows)]
unsafe extern "system" fn subclass_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERENTER, WM_POINTERLEAVE, WM_POINTERUP,
        WM_POINTERUPDATE,
    };

    if matches!(
        message,
        WM_POINTERENTER
            | WM_POINTERDOWN
            | WM_POINTERUPDATE
            | WM_POINTERUP
            | WM_POINTERLEAVE
            | WM_POINTERCAPTURECHANGED
    ) {
        // SAFETY: `wparam` and `lparam` come straight from the window message.
        unsafe { handle_pointer_message(message, wparam) };
    }
    // SAFETY: forwarding to the previous window procedure is always valid.
    unsafe { windows::Win32::UI::Shell::DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(windows)]
unsafe fn handle_pointer_message(message: u32, wparam: windows::Win32::Foundation::WPARAM) {
    use windows::Win32::UI::Input::Pointer::{
        GetPointerInfo, GetPointerPenInfo, POINTER_FLAG_INCONTACT, POINTER_INFO, POINTER_PEN_INFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        PT_PEN, WM_POINTERCAPTURECHANGED, WM_POINTERLEAVE, WM_POINTERUP,
    };

    let pointer_id = (wparam.0 & 0xFFFF) as u32;
    let mut info = POINTER_INFO::default();
    // SAFETY: `info` is a valid, properly aligned output buffer.
    if unsafe { GetPointerInfo(pointer_id, &mut info) }.is_err() {
        return;
    }
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static SEEN: AtomicBool = AtomicBool::new(false);
        if !SEEN.swap(true, Ordering::Relaxed) {
            bevy::log::info!(
                "pen input: first WM_POINTER message ({message}), type {:?}",
                info.pointerType
            );
        }
    }
    if info.pointerType != PT_PEN {
        return;
    }

    if matches!(
        message,
        WM_POINTERUP | WM_POINTERLEAVE | WM_POINTERCAPTURECHANGED
    ) {
        store(false, 0.0);
        return;
    }

    let mut pen = POINTER_PEN_INFO::default();
    // SAFETY: `pen` is a valid, properly aligned output buffer.
    let has_pen_info = unsafe { GetPointerPenInfo(pointer_id, &mut pen) }.is_ok();
    // PEN_MASK_PRESSURE, the bit Windows sets when `pressure` is meaningful.
    const PEN_MASK_PRESSURE: u32 = 1;
    let pressure = if has_pen_info && pen.penMask & PEN_MASK_PRESSURE != 0 {
        pen.pressure as f32 / 1024.0
    } else {
        0.0
    };
    let contact = info.pointerFlags.contains(POINTER_FLAG_INCONTACT);
    store(contact, pressure);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(detected: bool, contact: bool, pressure: f32, age: Duration) -> PenState {
        PenState {
            detected,
            contact,
            pressure,
            stamp: Instant::now().checked_sub(age),
        }
    }

    #[test]
    fn an_unseen_pen_returns_nothing() {
        assert_eq!(
            sample_from(state(false, true, 0.5, Duration::ZERO), Instant::now()),
            None
        );
    }

    #[test]
    fn a_fresh_sample_reports_pressure_and_contact() {
        assert_eq!(
            sample_from(
                state(true, true, 0.42, Duration::from_millis(50)),
                Instant::now()
            ),
            Some((true, 0.42))
        );
    }

    #[test]
    fn a_stale_sample_reports_released() {
        assert_eq!(
            sample_from(
                state(true, true, 0.42, Duration::from_secs(5)),
                Instant::now()
            ),
            Some((false, 0.0))
        );
    }

    #[test]
    fn a_detected_pen_stays_detected_while_idle() {
        assert_eq!(
            sample_from(
                state(true, false, 0.0, Duration::from_secs(30)),
                Instant::now()
            ),
            Some((false, 0.0))
        );
    }
}
