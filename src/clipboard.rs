/// Read the host system clipboard as text.
///
/// On macOS, prefer NSPasteboard directly. That avoids spawning `pbpaste`
/// from croft's already-threaded TUI process, which is both slower and more
/// brittle under macOS's fork-safety rules.
pub fn read_string() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if let Some(s) = macos::read_string_native() {
            return Some(s);
        }
    }

    read_pbpaste()
}

fn read_pbpaste() -> Option<String> {
    let out = std::process::Command::new("pbpaste").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_void};

    type ObjcId = *mut c_void;
    type Sel = *mut c_void;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> ObjcId;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
        fn objc_msgSend();
    }

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    pub fn read_string_native() -> Option<String> {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let result = read_string_native_inner();
            objc_autoreleasePoolPop(pool);
            result
        }
    }

    unsafe fn read_string_native_inner() -> Option<String> {
        unsafe {
            let pasteboard_class = class(b"NSPasteboard\0")?;
            let pasteboard = msg_send_id(pasteboard_class, sel(b"generalPasteboard\0")?);
            if pasteboard.is_null() {
                return None;
            }

            for type_name in [
                b"public.utf8-plain-text\0".as_slice(),
                b"public.plain-text\0".as_slice(),
                b"public.text\0".as_slice(),
                b"NSStringPboardType\0".as_slice(),
            ] {
                if let Some(value) = pasteboard_string_for_type(pasteboard, type_name) {
                    return Some(value);
                }
            }

            None
        }
    }

    unsafe fn pasteboard_string_for_type(
        pasteboard: ObjcId,
        type_name: &'static [u8],
    ) -> Option<String> {
        unsafe {
            let string_class = class(b"NSString\0")?;
            let pasteboard_type = msg_send_id_ptr(
                string_class,
                sel(b"stringWithUTF8String:\0")?,
                type_name.as_ptr().cast(),
            );
            if pasteboard_type.is_null() {
                return None;
            }

            let value = msg_send_id_id(pasteboard, sel(b"stringForType:\0")?, pasteboard_type);
            if value.is_null() {
                return None;
            }
            let utf8 = msg_send_ptr(value, sel(b"UTF8String\0")?);
            if utf8.is_null() {
                return None;
            }

            Some(CStr::from_ptr(utf8).to_string_lossy().into_owned())
        }
    }

    unsafe fn class(name: &'static [u8]) -> Option<ObjcId> {
        let c = unsafe { objc_getClass(name.as_ptr().cast()) };
        (!c.is_null()).then_some(c)
    }

    unsafe fn sel(name: &'static [u8]) -> Option<Sel> {
        let s = unsafe { sel_registerName(name.as_ptr().cast()) };
        (!s.is_null()).then_some(s)
    }

    unsafe fn msg_send_id(receiver: ObjcId, selector: Sel) -> ObjcId {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel) -> ObjcId =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector)
        }
    }

    unsafe fn msg_send_id_ptr(receiver: ObjcId, selector: Sel, arg: *const c_char) -> ObjcId {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel, *const c_char) -> ObjcId =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector, arg)
        }
    }

    unsafe fn msg_send_id_id(receiver: ObjcId, selector: Sel, arg: ObjcId) -> ObjcId {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel, ObjcId) -> ObjcId =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector, arg)
        }
    }

    unsafe fn msg_send_ptr(receiver: ObjcId, selector: Sel) -> *const c_char {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel) -> *const c_char =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector)
        }
    }
}
