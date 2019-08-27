use std::cmp::max;
use std::collections::HashMap;
use std::default::Default;
use std::ffi::CString;
use std::mem::{forget, MaybeUninit};
use std::os::raw::{c_int, c_uint, c_ulong, c_void};
use std::ptr::{null, null_mut};

use x11::xlib::*;

static mut WM_DETECTED: bool = false;

struct WindowManager {
    display: *mut Display,
    root: Window,
    clients: HashMap<Window, Window>,
    drag: DragInfo,
}

#[derive(Default)]
struct DragInfo {
    start_pos: (c_int, c_int),
    start_frame_pos: (c_int, c_int),
    start_frame_size: (c_int, c_int),
}

unsafe fn uninit<T>() -> T {
    MaybeUninit::uninit().assume_init()
}

impl Default for WindowManager {
    fn default() -> Self {
        let display = unsafe { XOpenDisplay(null()) };
        if display.is_null() {
            panic!("`XOpenDisplay()` failed!");
        } else {
            eprintln!(
                "Open display: \"{}\"",
                unsafe { CString::from_raw(XDisplayString(display)) }
                    .to_str()
                    .unwrap_or("`CString::to_str()` error!")
            );
        }

        Self {
            display,
            root: unsafe { XDefaultRootWindow(display) },
            clients: HashMap::new(),
            drag: DragInfo::default(),
        }
    }
}

impl WindowManager {
    unsafe extern "C" fn wm_detected(_display: *mut Display, err: *mut XErrorEvent) -> c_int {
        if (*err).error_code == BadAccess {
            WM_DETECTED = true;
        }
        0
    }

    unsafe extern "C" fn x_error(display: *mut Display, err: *mut XErrorEvent) -> c_int {
        const MAX_ERROR_TEXT_LENGTH: usize = 1024;
        let mut error_text = Vec::with_capacity(MAX_ERROR_TEXT_LENGTH);
        XGetErrorText(
            display,
            (*err).error_code as c_int,
            error_text.as_mut_ptr(),
            MAX_ERROR_TEXT_LENGTH as c_int,
        );
        eprintln!(
            "X error: {}",
            CString::from_raw(error_text.as_mut_ptr())
                .to_str()
                .unwrap_or("`CString::to_str()` error!")
        );
        forget(error_text);
        0
    }

    unsafe fn frame(&mut self, w: Window, created_before: bool) {
        const BORDER_WIDTH: c_uint = 3;
        const BORDER_COLOR: c_ulong = 0xff0000;
        const BG_COLOR: c_ulong = 0x0000ff;

        if self.clients.contains_key(&w) {
            return;
        }

        let mut x_window_attrs = uninit();
        XGetWindowAttributes(self.display, w, &mut x_window_attrs);
        if created_before {
            if x_window_attrs.override_redirect != 0 || x_window_attrs.map_state != IsViewable {
                return;
            }
        }

        let frame: Window = XCreateSimpleWindow(
            self.display,
            self.root,
            x_window_attrs.x,
            x_window_attrs.y,
            x_window_attrs.width as c_uint,
            x_window_attrs.height as c_uint,
            BORDER_WIDTH,
            BORDER_COLOR,
            BG_COLOR,
        );

        XSelectInput(
            self.display,
            frame,
            SubstructureRedirectMask | SubstructureNotifyMask,
        );

        XAddToSaveSet(self.display, w);
        XReparentWindow(self.display, w, frame, 0, 0);
        XMapWindow(self.display, frame);

        self.clients.insert(w, frame);

        XGrabButton(
            self.display,
            Button1,
            Mod1Mask,
            w,
            0,
            (ButtonPressMask | ButtonReleaseMask | ButtonMotionMask) as c_uint,
            GrabModeAsync,
            GrabModeAsync,
            0,
            0,
        );

        XGrabButton(
            self.display,
            Button3,
            Mod1Mask,
            w,
            0,
            (ButtonPressMask | ButtonReleaseMask | ButtonMotionMask) as c_uint,
            GrabModeAsync,
            GrabModeAsync,
            0,
            0,
        );

        XGrabKey(
            self.display,
            XKeysymToKeycode(self.display, x11::keysym::XK_F4 as c_ulong) as c_int,
            Mod1Mask,
            w,
            0,
            GrabModeAsync,
            GrabModeAsync,
        );
        eprintln!("Framed window: {} [{}]", w, frame);
    }

    unsafe fn unframe(&mut self, w: Window) {
        if !self.clients.contains_key(&w) {
            return;
        }

        let frame = self.clients[&w];
        XUnmapWindow(self.display, frame);
        XReparentWindow(self.display, w, self.root, 0, 0);
        XRemoveFromSaveSet(self.display, w);
        XDestroyWindow(self.display, frame);
        self.clients.remove(&w);
        eprintln!("Unframed window: {}", w);
    }

    unsafe fn map_request(&mut self, e: &XMapRequestEvent) {
        self.frame(e.window, false);
        XMapWindow(self.display, e.window);
    }

    unsafe fn unmap_notify(&mut self, e: &XUnmapEvent) {
        if e.event == self.root {
            return;
        }

        self.unframe(e.window);
    }

    unsafe fn configure_request(&self, e: &XConfigureRequestEvent) {
        let mut changes: XWindowChanges = uninit();
        changes.x = e.x;
        changes.y = e.y;
        changes.width = e.width;
        changes.height = e.height;
        changes.border_width = e.border_width;
        changes.sibling = e.above;
        changes.stack_mode = e.detail;

        if self.clients.contains_key(&e.window) {
            let frame = self.clients[&e.window];
            XConfigureWindow(self.display, frame, e.value_mask as c_uint, &mut changes);
            eprintln!("Resize [{}] to {}x{}", frame, e.width, e.height);
        }
        XConfigureWindow(self.display, e.window, e.value_mask as c_uint, &mut changes);
        eprintln!("Resize [{}] to {}x{}", e.window, e.width, e.height);
    }

    unsafe fn button_press(&mut self, e: &XButtonEvent) {
        if !self.clients.contains_key(&e.window) {
            return;
        }

        let frame = self.clients[&e.window];
        self.drag.start_pos = (e.x_root, e.y_root);

        let mut returned_root = uninit();
        let (mut x, mut y) = uninit();
        let (mut width, mut height, mut border_width, mut depth) = uninit();
        XGetGeometry(
            self.display,
            frame,
            &mut returned_root,
            &mut x,
            &mut y,
            &mut width,
            &mut height,
            &mut border_width,
            &mut depth,
        );
        self.drag.start_frame_pos = (x, y);
        self.drag.start_frame_size = (width as c_int, height as c_int);

        XRaiseWindow(self.display, frame);
    }

    unsafe fn motion_notify(&self, e: &XMotionEvent) {
        if !self.clients.contains_key(&e.window) {
            return;
        }

        let frame = self.clients[&e.window];
        let drag_pos = (e.x_root, e.y_root);
        let delta = (
            drag_pos.0 - self.drag.start_pos.0,
            drag_pos.1 - self.drag.start_pos.1,
        );

        if e.state & Button1Mask != 0 {
            let dest_frame_pos = (
                self.drag.start_frame_pos.0 + delta.0,
                self.drag.start_frame_pos.1 + delta.1,
            );
            XMoveWindow(self.display, frame, dest_frame_pos.0, dest_frame_pos.1);
        } else if e.state & Button3Mask != 0 {
            let size_delta = (
                max(delta.0, -self.drag.start_frame_size.0),
                max(delta.1, -self.drag.start_frame_size.1),
            );
            let dest_frame_size = (
                (self.drag.start_frame_size.0 + size_delta.0) as c_uint,
                (self.drag.start_frame_size.1 + size_delta.1) as c_uint,
            );

            XResizeWindow(self.display, frame, dest_frame_size.0, dest_frame_size.1);
            XResizeWindow(self.display, e.window, dest_frame_size.0, dest_frame_size.1);
        }
    }
}

fn main() {
    let mut wm = WindowManager::default();
    unsafe {
        XSetErrorHandler(Some(WindowManager::wm_detected));
        XSelectInput(
            wm.display,
            wm.root,
            SubstructureRedirectMask | SubstructureNotifyMask,
        );
        XSync(wm.display, 0);

        if WM_DETECTED {
            panic!("Detected another window manager on display!");
        }

        XSetErrorHandler(Some(WindowManager::x_error));
        XGrabServer(wm.display);

        let mut returned_root = 0;
        let mut returned_parent = 0;
        let mut top_level_windows = null_mut();
        let mut num_top_level_windows = 0;
        XQueryTree(
            wm.display,
            wm.root,
            &mut returned_root,
            &mut returned_parent,
            &mut top_level_windows,
            &mut num_top_level_windows,
        );
        assert_eq!(returned_root, wm.root);

        for i in 0..num_top_level_windows as usize {
            wm.frame(*top_level_windows.add(i), true);
        }
        XFree(top_level_windows as *mut c_void);
        XUngrabServer(wm.display);

        loop {
            let mut e = uninit();
            XNextEvent(wm.display, &mut e);

            #[allow(non_upper_case_globals)]
            match e.get_type() {
                MapRequest => wm.map_request(e.as_ref()),
                UnmapNotify => wm.unmap_notify(e.as_ref()),
                ConfigureRequest => wm.configure_request(e.as_ref()),
                ButtonPress => wm.button_press(e.as_ref()),
                MotionNotify => wm.motion_notify(e.as_ref()),
                _ => (),
            }
        }
    }
}
