#[cfg(windows)]
use once_cell::sync::Lazy;
#[cfg(windows)]
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
pub(crate) struct InputState {
    #[cfg(windows)]
    pointer_position: Option<egui::Pos2>,
    #[cfg(windows)]
    pointer_buttons: [bool; 3],
    #[cfg(windows)]
    key_states: [bool; 8],
}

impl InputState {
    pub(crate) fn collect(
        &mut self,
        window: Option<isize>,
        screen_rect: egui::Rect,
    ) -> egui::RawInput {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::{HWND, POINT};
            use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
                GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_LEFT, VK_MBUTTON,
                VK_MENU, VK_RBUTTON, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow};

            let Some(window) = window else {
                return egui::RawInput::default();
            };
            let window = window as HWND;
            let focused = unsafe { GetForegroundWindow() == window };
            let key_down = |key: u16| unsafe { GetAsyncKeyState(key as i32) < 0 };
            let modifiers = egui::Modifiers {
                alt: key_down(VK_MENU),
                ctrl: key_down(VK_CONTROL),
                shift: key_down(VK_SHIFT),
                mac_cmd: false,
                command: key_down(VK_CONTROL),
            };
            let mut events = Vec::new();
            let mut point = POINT::default();
            let has_position = focused
                && unsafe {
                    GetCursorPos(&mut point) != 0 && ScreenToClient(window, &mut point) != 0
                };
            let position = has_position.then(|| egui::pos2(point.x as f32, point.y as f32));

            if let Some(position) = position {
                if screen_rect.contains(position) {
                    events.push(egui::Event::PointerMoved(position));
                    self.pointer_position = Some(position);
                } else {
                    events.push(egui::Event::PointerGone);
                    self.pointer_position = Some(position);
                }
            } else {
                events.push(egui::Event::PointerGone);
            }

            let buttons = [
                (VK_LBUTTON, egui::PointerButton::Primary),
                (VK_RBUTTON, egui::PointerButton::Secondary),
                (VK_MBUTTON, egui::PointerButton::Middle),
            ];
            for (index, (virtual_key, button)) in buttons.into_iter().enumerate() {
                let pressed = focused && key_down(virtual_key);
                if pressed != self.pointer_buttons[index] {
                    events.push(egui::Event::PointerButton {
                        pos: self.pointer_position.unwrap_or(egui::Pos2::ZERO),
                        button,
                        pressed,
                        modifiers,
                    });
                    self.pointer_buttons[index] = pressed;
                }
            }

            let keys = [
                (VK_TAB, egui::Key::Tab),
                (VK_RETURN, egui::Key::Enter),
                (VK_SPACE, egui::Key::Space),
                (VK_ESCAPE, egui::Key::Escape),
                (VK_LEFT, egui::Key::ArrowLeft),
                (VK_RIGHT, egui::Key::ArrowRight),
                (VK_UP, egui::Key::ArrowUp),
                (VK_DOWN, egui::Key::ArrowDown),
            ];
            for (index, (virtual_key, key)) in keys.into_iter().enumerate() {
                let pressed = focused && key_down(virtual_key);
                if pressed != self.key_states[index] {
                    events.push(egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                    self.key_states[index] = pressed;
                }
            }

            egui::RawInput {
                focused,
                modifiers,
                events,
                ..Default::default()
            }
        }

        #[cfg(not(windows))]
        {
            let _ = (window, screen_rect);
            egui::RawInput::default()
        }
    }
}

#[cfg(windows)]
const WINDOW_SUBCLASS_ID: usize = 0x564B_4342;

#[cfg(windows)]
#[derive(Clone, Copy, Default)]
struct WindowCaptureState {
    references: usize,
    pointer: bool,
    keyboard: bool,
}

#[cfg(windows)]
static WINDOW_CAPTURES: Lazy<Mutex<HashMap<isize, WindowCaptureState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[cfg(windows)]
pub(crate) fn install_window_input(window: isize) -> bool {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;

    {
        let mut captures = WINDOW_CAPTURES.lock().unwrap();
        if let Some(state) = captures.get_mut(&window) {
            state.references += 1;
            return true;
        }
    }
    if unsafe {
        SetWindowSubclass(
            window as HWND,
            Some(window_subclass_proc),
            WINDOW_SUBCLASS_ID,
            0,
        )
    } == 0
    {
        return false;
    }
    WINDOW_CAPTURES.lock().unwrap().insert(
        window,
        WindowCaptureState {
            references: 1,
            ..Default::default()
        },
    );
    true
}

#[cfg(windows)]
pub(crate) fn uninstall_window_input(window: isize) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;

    let should_remove = {
        let mut captures = WINDOW_CAPTURES.lock().unwrap();
        let Some(state) = captures.get_mut(&window) else {
            return;
        };
        state.references -= 1;
        if state.references == 0 {
            captures.remove(&window);
            true
        } else {
            false
        }
    };
    if should_remove {
        unsafe {
            RemoveWindowSubclass(
                window as HWND,
                Some(window_subclass_proc),
                WINDOW_SUBCLASS_ID,
            )
        };
    }
}

#[cfg(windows)]
pub(crate) fn update_window_capture(window: isize, pointer: bool, keyboard: bool) {
    if let Some(state) = WINDOW_CAPTURES.lock().unwrap().get_mut(&window) {
        state.pointer = pointer;
        state.keyboard = keyboard;
    }
}

#[cfg(windows)]
unsafe extern "system" fn window_subclass_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    _reference_data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_CHAR, WM_DEADCHAR, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK,
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_RBUTTONDBLCLK,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
        WM_UNICHAR, WM_XBUTTONDBLCLK, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    let window = hwnd as isize;
    if message == WM_NCDESTROY {
        WINDOW_CAPTURES.lock().unwrap().remove(&window);
        unsafe { RemoveWindowSubclass(hwnd, Some(window_subclass_proc), WINDOW_SUBCLASS_ID) };
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }

    let capture = WINDOW_CAPTURES
        .lock()
        .unwrap()
        .get(&window)
        .copied()
        .unwrap_or_default();
    let pointer_message = matches!(
        message,
        WM_MOUSEMOVE
            | WM_LBUTTONDOWN
            | WM_LBUTTONUP
            | WM_LBUTTONDBLCLK
            | WM_RBUTTONDOWN
            | WM_RBUTTONUP
            | WM_RBUTTONDBLCLK
            | WM_MBUTTONDOWN
            | WM_MBUTTONUP
            | WM_MBUTTONDBLCLK
            | WM_XBUTTONDOWN
            | WM_XBUTTONUP
            | WM_XBUTTONDBLCLK
            | WM_MOUSEWHEEL
            | WM_MOUSEHWHEEL
    );
    let keyboard_message = matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_SYSCHAR
            | WM_DEADCHAR
            | WM_SYSDEADCHAR
            | WM_UNICHAR
    );
    if (capture.pointer && pointer_message) || (capture.keyboard && keyboard_message) {
        return 0;
    }
    // Raw input still has to reach DefWindowProc for cleanup, just not the app.
    if message == WM_INPUT && (capture.pointer || capture.keyboard) {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
