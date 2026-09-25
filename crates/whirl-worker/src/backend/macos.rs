//! The macOS setter: `-[NSWorkspace setDesktopImageURL:forScreen:options:error:]`,
//! sent through the Objective-C runtime by hand.
//!
//! **Why the FFI is hand-written.** v0.1 allows no third-party dependency at all
//! (docs/development.md section 2), and the CI `guards` job fails if any package
//! other than the four workspace crates appears in `Cargo.lock`, so the `objc2`
//! and `cocoa` crates are not options. What is left is `objc_msgSend`
//! transmuted at each call site to the exact signature of that call: the
//! arguments and the return value travel in the registers and stack slots the
//! ABI chose for the real signature, so a transmute that does not match it is
//! undefined behaviour rather than a crash the compiler or a test would catch.
//! Every call below carries the Objective-C declaration it mirrors, and every
//! transmute says why it is transposed the way it is.
//!
//! **Nothing here is AppKit as a library.** No window, no `NSApplication`, no run
//! loop, no options dictionary: the call needs an Aqua session, not a UI process
//! (docs/architecture.md 3.2), and `whirld` never links this file
//! (docs/architecture.md R2: the daemon does not depend on the worker's platform
//! crate).
//!
//! **Absolute paths only, checked before any call.** `fileURLWithPath:` resolves
//! a relative path against this process's working directory, which is exactly the
//! prototype's `/wh-commons.jpg` incident (docs/architecture.md 3.2), so a
//! relative path is refused here instead of silently setting something else.

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;

use super::SetError;
use whirl_core::protocol::ErrorCode;

/// An Objective-C object reference: `id`, which is a pointer to an opaque struct.
type Id = *mut c_void;
/// A selector: `SEL`, an opaque pointer the runtime hands back for a name.
type Sel = *mut c_void;

/// `-[NSWorkspace setDesktopImageURL:forScreen:options:error:]`, declared at
/// `NSWorkspace.h:227` for macOS 10.6 and never deprecated.
const SELECTOR_SET: &CStr = c"setDesktopImageURL:forScreen:options:error:";

// The Objective-C runtime. `objc_msgSend` is declared here with no arguments and
// no return because there is no single prototype for it in C either: every
// message has its own signature, and the transmute at each call site is what
// gives the symbol the ABI of the message about to be sent.
#[link(name = "objc")]
unsafe extern "C" {
    /// `id objc_msgSend(id self, SEL _cmd, ...)`. Never called through this
    /// declaration, only through a transmuted pointer at the call site.
    fn objc_msgSend();
    /// `Class objc_getClass(const char *name)`: null when the class is not
    /// registered, which is how a framework that failed to load shows up.
    fn objc_getClass(name: *const c_char) -> Id;
    /// `SEL sel_registerName(const char *name)`.
    fn sel_registerName(name: *const c_char) -> Sel;
    /// `void *objc_autoreleasePoolPush(void)`, the entry point `@autoreleasepool`
    /// compiles to. Exported by libobjc alongside `objc_msgSend`; the other
    /// spelling of the same thing is the deprecated `NSAutoreleasePool` class.
    fn objc_autoreleasePoolPush() -> *mut c_void;
    /// `void objc_autoreleasePoolPop(void *pool)`.
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// AppKit supplies `NSWorkspace` and `NSScreen`; Foundation supplies `NSString`,
// `NSURL` and `NSError`. Not one symbol is referenced from either framework, and
// that is deliberate: the classes are reached with `objc_getClass`, so what the
// linker has to be told is the *framework*, so that the class exists in this
// process before the first lookup. Drop `-framework AppKit` and
// `objc_getClass("NSWorkspace")` answers null, which is the failure `class`
// reports rather than dereferencing.
//
// One block per framework: `#[link]` twice on one block is the same attribute
// written twice as far as `clippy::duplicated_attributes` is concerned, and it is
// a deny-by-default lint under `-D warnings`.
//
// Comments and not doc comments: rustdoc generates nothing for an extern block,
// and `-D warnings` counts `unused_doc_comments`.
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// An autorelease pool for everything the calls below create: the `NSString` from
/// the path, the `NSURL`, and any `NSError`. Without one the runtime logs
/// "autoreleased with no pool in place - just leaking" on stderr, and this
/// process's stderr is the worker's failure channel (docs/architecture.md 1.6),
/// so a stray line there is a contract break, not noise. It drains on drop, so
/// every early return above it is covered too.
struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    fn new() -> AutoreleasePool {
        AutoreleasePool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// `sel_registerName` for a Rust C-string literal: interned by the runtime and
/// never freed, so a `&'static CStr` is all it needs and there is no leak to
/// track.
fn selector(name: &'static CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}

/// `objc_getClass`, with the null case turned into a sentence instead of a
/// dereference. Every selector below is sent to a receiver that this function or
/// another message produced.
fn class(name: &'static CStr) -> Result<Id, String> {
    let found = unsafe { objc_getClass(name.as_ptr()) };
    if found.is_null() {
        return Err(format!(
            "the Objective-C class {} is not registered, so the framework that defines it is not loaded into this process",
            name.to_string_lossy()
        ));
    }
    Ok(found)
}

/// `[receiver selector]` where the message takes no arguments and returns an
/// object: the ABI is `id (*)(id, SEL)`. Used for class messages
/// (`[NSWorkspace sharedWorkspace]`, `[NSScreen screens]`) and instance messages
/// (`[url path]`, `[error domain]`) alike; only the receiver differs, never the
/// ABI. Sending to null is legal in Objective-C and answers null, so a null
/// receiver cannot become undefined behaviour here.
fn msg_id(receiver: Id, cmd: Sel) -> Id {
    let send: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd) }
}

/// `[receiver selector]` returning `NSUInteger`, which is `unsigned long`: 64
/// bits on both Darwin ABIs, hence `usize`. `[NSScreen.screens count]` is the one
/// caller.
fn msg_count(receiver: Id, cmd: Sel) -> usize {
    let send: unsafe extern "C" fn(Id, Sel) -> usize =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd) }
}

/// `[receiver selector: argument]` where the argument and the return are objects:
/// `id (*)(id, SEL, id)`. Used for `[NSString stringWithUTF8String:]`,
/// `[NSURL fileURLWithPath:]` and `[workspace desktopImageURLForScreen:]`.
fn msg_id1(receiver: Id, cmd: Sel, argument: Id) -> Id {
    let send: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd, argument) }
}

/// `[NSString stringWithUTF8String: c_string]`, whose argument is a
/// `const char *` rather than an object. Both are pointers and travel in the same
/// register, so the transmute is the same shape as [`msg_id1`]'s with the last
/// argument transposed to `*const c_char` — which is also why the pointer's
/// mutability is not the ABI's business and the cast at the call site is sound.
fn msg_string_with_utf8(receiver: Id, cmd: Sel, value: *const c_char) -> Id {
    let send: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd, value) }
}

/// `[array objectAtIndex: index]`, whose argument is an `NSUInteger` and not an
/// object: `NSUInteger` is `unsigned long`, 64 bits on both Darwin ABIs, hence
/// `usize`. Sharing [`msg_id1`] here would pass the index as a pointer width by
/// luck and as a different type by contract, which is the class of mistake this
/// file exists to not make.
fn msg_object_at(receiver: Id, cmd: Sel, index: usize) -> Id {
    let send: unsafe extern "C" fn(Id, Sel, usize) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd, index) }
}

/// `[NSError code]` returning `NSInteger`: `long`, signed, 64 bits on both Darwin
/// ABIs, hence `i64`.
fn msg_integer(receiver: Id, cmd: Sel) -> i64 {
    let send: unsafe extern "C" fn(Id, Sel) -> i64 =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd) }
}

/// `[NSString UTF8String]`: a NUL-terminated C string borrowed from the string.
fn msg_utf8(receiver: Id, cmd: Sel) -> *const c_char {
    let send: unsafe extern "C" fn(Id, Sel) -> *const c_char =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(receiver, cmd) }
}

/// `[receiver setDesktopImageURL:url forScreen:screen options:nil error:&error]`,
/// mirroring `- (BOOL)setDesktopImageURL:(NSURL *)url forScreen:(NSScreen *)screen
/// options:(NSDictionary<NSWorkspaceDesktopImageOptionKey, id> *)options
/// error:(NSError **)error` (`NSWorkspace.h:227`).
///
/// ABI, argument by argument: the receiver and the selector come first as for
/// every message, then the four arguments in the order the declaration lists
/// them: two objects, the options dictionary (null here), and the address of an
/// `NSError *` the callee may write through. The return is `BOOL`, which is
/// `signed char` on both Darwin ABIs, so `i8`; it is read as `!= 0` rather than
/// as a Rust `bool`, because `bool` is defined for 0 and 1 and an Objective-C
/// `BOOL` is not.
///
/// This stays on plain `objc_msgSend`: on x86_64 `objc_msgSend_stret` is the
/// entry point for a message that *returns* a struct and `objc_msgSend_fpret` for
/// one returning `long double`, and this one returns a byte.
///
/// The options dictionary is null on purpose. `allSpaces` exists in it and is
/// inert (docs/architecture.md 3.2: two nodes were measured with it and without
/// it), so it is not set here because the name looks right.
fn msg_set_desktop_image(workspace: Id, url: Id, screen: Id, error: *mut Id) -> bool {
    let send: unsafe extern "C" fn(Id, Sel, Id, Id, Id, *mut Id) -> i8 =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe {
        send(
            workspace,
            selector(SELECTOR_SET),
            url,
            screen,
            ptr::null_mut(),
            error,
        ) != 0
    }
}

/// `[NSWorkspace sharedWorkspace]`, the receiver both public functions use. A
/// class message, so the receiver is the class: no instance exists to be had
/// otherwise, and no `NSApplication` is needed for it (docs/architecture.md 3.2:
/// an Aqua session, not a UI process).
fn workspace() -> Result<Id, String> {
    let ns_workspace = class(c"NSWorkspace")?;
    Ok(msg_id(ns_workspace, selector(c"sharedWorkspace")))
}

/// `[NSScreen screens]`, every attached screen, a class message
/// (`NSScreen.h:25`: "All screens; first one is 'zero' screen"). The array is a
/// snapshot taken once per call: nothing here re-enumerates, so a screen that
/// disappears between the snapshot and its own call fails that call and cannot
/// shift the identity of another.
fn screens() -> Result<Vec<Id>, String> {
    let ns_screen = class(c"NSScreen")?;
    let array = msg_id(ns_screen, selector(c"screens"));
    if array.is_null() {
        return Err("NSScreen.screens answered null".to_string());
    }
    let count = msg_count(array, selector(c"count"));
    let mut found = Vec::with_capacity(count);
    for index in 0..count {
        found.push(msg_object_at(array, selector(c"objectAtIndex:"), index));
    }
    Ok(found)
}

/// `[NSString stringWithUTF8String:]` for a path that is already UTF-8, because a
/// Rust `&str` is. The C string is built with `CString` and not handed over as the
/// `&str`'s pointer: `stringWithUTF8String:` takes a NUL-terminated C string and a
/// Rust `&str` is bytes plus a length, with no terminator guaranteed after the
/// last byte. Passing the `&str`'s pointer is what makes the platform read past
/// the end of the path: measured here as an `NSError` domain that ran on into the
/// neighbouring string literals of the test binary. `CString` is also the second
/// NUL guard, and [`set`] keeps the first one so that the message can name the
/// path.
fn nsstring(value: &str) -> Result<Id, String> {
    let c_string = CString::new(value)
        .map_err(|_| "the value holds a NUL byte, so it cannot be a C string".to_string())?;
    let ns_string = class(c"NSString")?;
    let string = msg_string_with_utf8(
        ns_string,
        selector(c"stringWithUTF8String:"),
        c_string.as_ptr(),
    );
    if string.is_null() {
        return Err("NSString.stringWithUTF8String: answered null".to_string());
    }
    Ok(string)
}

/// `[NSURL fileURLWithPath:]`, the file URL `setDesktopImageURL:` requires. The
/// path is absolute by the time this is called.
fn file_url(path: &str) -> Result<Id, String> {
    let ns_url = class(c"NSURL")?;
    let string = nsstring(path)?;
    let url = msg_id1(ns_url, selector(c"fileURLWithPath:"), string);
    if url.is_null() {
        return Err("NSURL.fileURLWithPath: answered null".to_string());
    }
    Ok(url)
}

/// The object behind an object-returning message, as a Rust `String`. Null is
/// `None`, not an empty string: "the platform has nothing here" and "the platform
/// says the empty string" are different answers, and only one of them is a path.
fn string_of(receiver: Id, cmd: Sel) -> Option<String> {
    let value = msg_id(receiver, cmd);
    if value.is_null() {
        return None;
    }
    let utf8 = msg_utf8(value, selector(c"UTF8String"));
    if utf8.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// A screen's name for the log, from `-[NSScreen localizedName]` (macOS 10.15),
/// falling back to the position in `NSScreen.screens` when the platform answers
/// null. The position is a fallback for the message only: no call is ever made
/// through it.
fn screen_name(screen: Id, index: usize) -> String {
    string_of(screen, selector(c"localizedName"))
        .unwrap_or_else(|| format!("screen #{} (the platform reported no name)", index + 1))
}

/// The facts the failure message needs, read out of the `NSError` the platform
/// wrote through the out-parameter. Every field is `None` when the call answered
/// `NO` and left no error object behind, which the message says out loud rather
/// than filling in a plausible domain.
struct NsFailure {
    domain: Option<String>,
    code: Option<i64>,
    description: Option<String>,
}

fn ns_failure(error: Id) -> NsFailure {
    if error.is_null() {
        return NsFailure {
            domain: None,
            code: None,
            description: None,
        };
    }
    NsFailure {
        domain: string_of(error, selector(c"domain")),
        code: Some(msg_integer(error, selector(c"code"))),
        description: string_of(error, selector(c"localizedDescription")),
    }
}

/// The message a refused set carries: the NSError's domain and code, its own
/// sentence (docs/architecture.md 4's error table quotes it, "The file doesn't
/// exist."), the selector, the screen and the path. Pure, so the shape of the
/// mapping is testable on a machine with no desktop, and the one place a missing
/// field is spelled out.
fn describe(failure: &NsFailure, screen: &str, path: &str) -> String {
    format!(
        "{} failed on screen {} for {}: NSError domain {} code {} ({})",
        SELECTOR_SET.to_string_lossy(),
        screen,
        path,
        failure.domain.as_deref().unwrap_or("-"),
        failure
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_string()),
        failure
            .description
            .as_deref()
            .unwrap_or("no NSError was returned"),
    )
}

/// A refused set that never reached the platform: the call could not be built or
/// there was nothing to send it to, so the message says why and carries the path
/// it was asked for. Always `SetFailed`: nothing was set.
fn not_set(reason: String, path: &str) -> SetError {
    SetError::new(
        ErrorCode::SetFailed,
        format!("{reason}, so {path} was not set"),
    )
}

/// Put `path` on the desktop of every attached screen.
///
/// One call per screen, in `NSScreen.screens` order, all with the cached file's
/// absolute path (docs/architecture.md 3.2). Any `NSError` from any screen is
/// returned as `SetFailed` carrying that screen's domain and code: a set that
/// reached some screens and failed on one is a failure, not a success, because
/// the caller asked for one image everywhere and must not be told it got it.
///
/// This is the one call behind the backend boundary (docs/development.md section
/// 7) and the only place the noop and native paths differ.
pub fn set(path: &str) -> Result<(), SetError> {
    let _pool = AutoreleasePool::new();

    if !path.starts_with('/') {
        return Err(not_set(
            format!(
                "the macOS setter needs an absolute path and {path:?} is not one: \
                 NSURL.fileURLWithPath: would resolve it against this process's working directory"
            ),
            path,
        ));
    }
    if path.contains('\0') {
        return Err(not_set(
            format!("the macOS setter cannot set {path:?}: the path holds a NUL byte"),
            path,
        ));
    }

    let workspace = workspace().map_err(|reason| not_set(reason, path))?;
    let screens = screens().map_err(|reason| not_set(reason, path))?;
    if screens.is_empty() {
        return Err(not_set(
            "the macOS setter found no screens: NSScreen.screens is empty, \
             so this process has no Aqua session"
                .to_string(),
            path,
        ));
    }
    let url = file_url(path).map_err(|reason| not_set(reason, path))?;

    for (index, screen) in screens.iter().enumerate() {
        let screen = *screen;
        let name = screen_name(screen, index);
        let mut error: Id = ptr::null_mut();
        if !msg_set_desktop_image(workspace, url, screen, &mut error) {
            return Err(SetError::new(
                ErrorCode::SetFailed,
                describe(&ns_failure(error), &name, path),
            ));
        }
    }
    Ok(())
}
