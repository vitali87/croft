/// Read the host system clipboard as text.
///
/// On macOS, prefer NSPasteboard directly. That avoids spawning `pbpaste`
/// from croft's already-threaded TUI process, which is both slower and more
/// brittle under macOS's fork-safety rules.
///
/// On Linux, try `wl-paste` (Wayland) then `xclip` then `xsel` (X11).
pub fn read_string() -> Option<String> {
    #[cfg(test)]
    {
        if !test_clip::real_clipboard_opt_in() {
            return test_clip::mock_read();
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(s) = macos::read_string_native() {
            return Some(s);
        }
        return read_pbpaste();
    }
    #[cfg(target_os = "linux")]
    {
        linux::read_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_pbpaste() -> Option<String> {
    let out = std::process::Command::new("pbpaste").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Put `text` on the host system clipboard as UTF-8 plain text. Returns
/// `true` when the write was acknowledged by the OS pasteboard.
///
/// On macOS we go straight to `NSPasteboard.setString:forType:` so the
/// embedded TUI's Cmd+C lands on the system clipboard with the same
/// guarantees as a native Cocoa app — independent of whether the host
/// terminal (iTerm2 etc.) chooses to honour OSC 52, which empirically
/// it does not in every environment even with `AllowClipboardAccess` set.
/// `pbcopy` is the second fall-back so we cover the case where the
/// objc runtime resolves but `NSPasteboard` rejects the write for some
/// sandboxing reason.
///
/// On Linux, try `wl-copy` (Wayland) then `xclip` then `xsel` (X11).
/// Returns `false` only when none of those tools are available, in which
/// case the caller falls back to OSC 52 (useful for remote SSH sessions).
pub fn write_string(text: &str) -> bool {
    // NSPasteboard / NSString cannot carry an embedded NUL — `stringWithUTF8String:`
    // truncates at the first 0x00 — and silently truncating user data is far
    // worse than refusing the write. Reject up front so the caller can decide
    // what to do (terminal selection text already maps NUL to space in
    // `extract_selection_text`, so this branch is defensive, not load-bearing).
    if text.as_bytes().contains(&0) {
        return false;
    }
    #[cfg(test)]
    {
        if !test_clip::real_clipboard_opt_in() {
            return test_clip::mock_write(text);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if macos::write_string_native(text) {
            return true;
        }
        return write_pbcopy(text);
    }
    #[cfg(target_os = "linux")]
    {
        linux::write_string(text)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Write `text` to the system clipboard on Linux.
    ///
    /// Tries `wl-copy` (Wayland), then `xclip`, then `xsel`. Returns `true`
    /// when one of them succeeded. Returns `false` when none are installed,
    /// which happens in headless SSH sessions — the caller sends OSC 52 then.
    pub fn write_string(text: &str) -> bool {
        write_via(&["wl-copy"], text)
            || write_via(&["xclip", "-selection", "clipboard"], text)
            || write_via(&["xsel", "--clipboard", "--input"], text)
    }

    fn write_via(argv: &[&str], text: &str) -> bool {
        let Ok(mut child) = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.wait();
            return false;
        };
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.wait();
            return false;
        }
        drop(stdin);
        child.wait().map(|s| s.success()).unwrap_or(false)
    }

    /// Read text from the system clipboard on Linux.
    ///
    /// Tries `wl-paste` (Wayland), then `xclip`, then `xsel`.
    pub fn read_string() -> Option<String> {
        read_via(&["wl-paste"])
            .or_else(|| read_via(&["xclip", "-selection", "clipboard", "-o"]))
            .or_else(|| read_via(&["xsel", "--clipboard", "--output"]))
    }

    fn read_via(argv: &[&str]) -> Option<String> {
        let out = Command::new(argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

#[cfg(target_os = "macos")]
fn write_pbcopy(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.wait();
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        let _ = child.wait();
        return false;
    }
    drop(stdin);
    child.wait().map(|st| st.success()).unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub(crate) mod macos {
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

    pub fn write_string_native(text: &str) -> bool {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let ok = write_string_inner(text);
            objc_autoreleasePoolPop(pool);
            ok
        }
    }

    /// Wipe NSPasteboard's contents. Used by the test guard's drop path
    /// to wipe a leftover test sentinel when no prior clipboard text was
    /// snapshotted at acquire-time.
    #[cfg(test)]
    pub fn clear_native() {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            clear_inner();
            objc_autoreleasePoolPop(pool);
        }
    }

    #[cfg(test)]
    unsafe fn clear_inner() {
        unsafe {
            let Some(pb_class) = class(b"NSPasteboard\0") else {
                return;
            };
            let Some(general_sel) = sel(b"generalPasteboard\0") else {
                return;
            };
            let pasteboard = msg_send_id(pb_class, general_sel);
            if pasteboard.is_null() {
                return;
            }
            let Some(clear_sel) = sel(b"clearContents\0") else {
                return;
            };
            let _ = msg_send_isize(pasteboard, clear_sel);
        }
    }

    unsafe fn write_string_inner(text: &str) -> bool {
        unsafe {
            let Some(pb_class) = class(b"NSPasteboard\0") else {
                return false;
            };
            let Some(general_sel) = sel(b"generalPasteboard\0") else {
                return false;
            };
            let pasteboard = msg_send_id(pb_class, general_sel);
            if pasteboard.is_null() {
                return false;
            }

            let Some(clear_sel) = sel(b"clearContents\0") else {
                return false;
            };
            let _ = msg_send_isize(pasteboard, clear_sel);

            let Some(nsstring_class) = class(b"NSString\0") else {
                return false;
            };
            let Some(utf8_sel) = sel(b"stringWithUTF8String:\0") else {
                return false;
            };

            let Ok(payload) = std::ffi::CString::new(text) else {
                return false;
            };
            let value = msg_send_id_ptr(nsstring_class, utf8_sel, payload.as_ptr());
            if value.is_null() {
                return false;
            }

            let Ok(type_cstr) = std::ffi::CString::new("public.utf8-plain-text") else {
                return false;
            };
            let type_nsstr = msg_send_id_ptr(nsstring_class, utf8_sel, type_cstr.as_ptr());
            if type_nsstr.is_null() {
                return false;
            }

            let Some(set_string_sel) = sel(b"setString:forType:\0") else {
                return false;
            };
            msg_send_bool_id_id(pasteboard, set_string_sel, value, type_nsstr)
        }
    }

    unsafe fn msg_send_isize(receiver: ObjcId, selector: Sel) -> isize {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel) -> isize =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector)
        }
    }

    unsafe fn msg_send_bool_id_id(
        receiver: ObjcId,
        selector: Sel,
        arg1: ObjcId,
        arg2: ObjcId,
    ) -> bool {
        unsafe {
            let f: unsafe extern "C" fn(ObjcId, Sel, ObjcId, ObjcId) -> u8 =
                std::mem::transmute(objc_msgSend as *const ());
            f(receiver, selector, arg1, arg2) != 0
        }
    }
}

/// Test-only support for keeping clipboard tests deterministic.
///
/// Under `cargo test` the binary's tests run on a shared thread pool. If
/// `write_string` always went to the real `NSPasteboard`, every test in
/// the crate that exercises a copy gesture (terminal copy, editor copy,
/// editor cut, kill-to-EOL, ring-yank, …) would stomp on the host
/// clipboard concurrently with the clipboard regression tests, and the
/// round-trip assertions would flake on whichever write landed last.
///
/// The chosen design: `write_string` and `read_string` route to a
/// per-thread mock by default during tests. Tests that need to prove the
/// real macOS pasteboard path (the FFI bridge in `mod macos`) opt in
/// explicitly via `lock_clipboard_for_test`, which:
///   * Acquires a process-global `Mutex` so only one test at a time
///     drives the real OS clipboard.
///   * Flips the per-thread `REAL_OPT_IN` cell to `true` so
///     `write_string` / `read_string` skip the mock for the duration
///     of the guard.
#[cfg(test)]
pub(crate) mod test_clip {
    use std::cell::{Cell, RefCell};
    use std::sync::{Mutex, MutexGuard};

    pub(crate) static CLIP_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static REAL_OPT_IN: Cell<bool> = const { Cell::new(false) };
        static MOCK: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(crate) fn real_clipboard_opt_in() -> bool {
        REAL_OPT_IN.with(|c| c.get())
    }

    pub(crate) fn mock_write(text: &str) -> bool {
        MOCK.with(|b| *b.borrow_mut() = Some(text.to_string()));
        true
    }

    pub(crate) fn mock_read() -> Option<String> {
        MOCK.with(|b| b.borrow().clone())
    }

    /// Guard returned by `lock_clipboard_for_test`. While held, the
    /// current thread's `write_string` / `read_string` calls bypass the
    /// per-thread mock and exercise the real macOS pasteboard, and no
    /// other thread can hold the lock concurrently.
    ///
    /// On macOS the guard also snapshots the system clipboard at
    /// acquire-time and restores it on drop, so a `cargo test` run never
    /// leaves a test sentinel (e.g. `croft-terminal-cmdc-<pid>`) sitting
    /// on the developer's real clipboard for the next paste to surface.
    pub(crate) struct ClipboardTestGuard {
        _mutex: MutexGuard<'static, ()>,
        #[cfg(target_os = "macos")]
        snapshot: Option<String>,
    }

    impl ClipboardTestGuard {
        pub(crate) fn acquire() -> Self {
            let mutex = CLIP_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            #[cfg(target_os = "macos")]
            let snapshot = super::macos::read_string_native();
            REAL_OPT_IN.with(|c| c.set(true));
            Self {
                _mutex: mutex,
                #[cfg(target_os = "macos")]
                snapshot,
            }
        }
    }

    impl Drop for ClipboardTestGuard {
        fn drop(&mut self) {
            #[cfg(target_os = "macos")]
            {
                match self.snapshot.take() {
                    Some(prev) => {
                        let _ = super::macos::write_string_native(&prev);
                    }
                    None => {
                        super::macos::clear_native();
                    }
                }
            }
            REAL_OPT_IN.with(|c| c.set(false));
        }
    }
}

#[cfg(test)]
pub(crate) fn lock_clipboard_for_test() -> test_clip::ClipboardTestGuard {
    test_clip::ClipboardTestGuard::acquire()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn lock_clipboard() -> test_clip::ClipboardTestGuard {
        lock_clipboard_for_test()
    }

    fn unique_sentinel(label: &str) -> String {
        format!(
            "croft-clip-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    #[test]
    fn write_string_lands_text_on_nspasteboard_for_pbpaste_to_read() {
        let _g = lock_clipboard();
        let sentinel = unique_sentinel("plain");
        assert!(
            write_string(&sentinel),
            "write_string must succeed on macOS via NSPasteboard"
        );
        let got = read_string().expect("read_string must surface clipboard text");
        assert_eq!(
            got, sentinel,
            "what we just wrote with NSPasteboard.setString:forType: must be exactly what NSPasteboard.stringForType: returns next - if this ever drifts, Cmd+C in the croft terminal silently fails for the user"
        );
    }

    #[test]
    fn write_string_overwrites_previous_clipboard_contents() {
        let _g = lock_clipboard();
        let first = unique_sentinel("first");
        let second = unique_sentinel("second");
        assert!(write_string(&first));
        assert!(write_string(&second));
        assert_eq!(
            read_string().as_deref(),
            Some(second.as_str()),
            "the second copy must overwrite the first - clearContents+setString:forType: must not append"
        );
    }

    #[test]
    fn write_string_round_trips_unicode_payloads() {
        let _g = lock_clipboard();
        let sentinel = format!("héllo · 世界 · 🦀 · {}", unique_sentinel("unicode"));
        assert!(write_string(&sentinel));
        assert_eq!(
            read_string().as_deref(),
            Some(sentinel.as_str()),
            "multi-byte UTF-8 must round-trip - the embedded shell happily produces emoji and CJK and the clipboard must not mangle them"
        );
    }

    #[test]
    fn write_string_round_trips_multi_line_payloads() {
        let _g = lock_clipboard();
        let body = "first\nsecond\nthird";
        let sentinel = format!("{body}-{}", unique_sentinel("multiline"));
        assert!(write_string(&sentinel));
        let got = read_string().expect("clipboard read");
        assert_eq!(
            got, sentinel,
            "newline-separated terminal selections must round-trip verbatim"
        );
    }

    #[test]
    fn write_string_rejects_payloads_containing_a_nul_byte_without_panicking() {
        let _g = lock_clipboard();
        let baseline = unique_sentinel("baseline");
        assert!(write_string(&baseline));
        let with_nul = "before\0after";
        assert!(
            !write_string(with_nul),
            "embedded nul byte must surface as a clean false from write_string instead of panicking inside the FFI bridge"
        );
        assert_eq!(
            read_string().as_deref(),
            Some(baseline.as_str()),
            "a failed write must not clobber the previous clipboard contents"
        );
    }

    #[test]
    fn macos_native_write_string_succeeds_directly() {
        let _g = lock_clipboard();
        let sentinel = unique_sentinel("native-direct");
        assert!(
            macos::write_string_native(&sentinel),
            "the NSPasteboard FFI bridge itself must succeed on macOS - if this fails, pbcopy will be the production fallback and we'll know the bridge regressed"
        );
        assert_eq!(read_string().as_deref(), Some(sentinel.as_str()));
    }
}
