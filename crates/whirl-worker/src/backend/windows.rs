//! The Windows setter and readback: `IDesktopWallpaper`, called through
//! hand-written COM.
//!
//! **Why the interop is hand-written.** v0.1 allows no third-party dependency at
//! all (docs/development.md section 2), and the CI `guards` job fails if any
//! package other than the four workspace crates appears in `Cargo.lock`, so the
//! `windows` crate and `winapi` are not options. What is left is the interface by
//! hand: the two `GUID`s, the three `ole32` entry points, and
//! `IDesktopWallpaper`'s vtable written out in the order the interface declares
//! its methods.
//!
//! **The vtable order is the thing that fails.** A COM vtable is a struct of
//! function pointers and a method's slot is its position among the interface's
//! methods, `IUnknown`'s three first. A hand-written table calls whatever pointer
//! sits at that index, so a slot placed one early or one late is a call into a
//! different method whose signature can still look right: it compiles, and it
//! corrupts. Every slot below is in the order `IDesktopWallpaper` declares its
//! sixteen methods in `shobjidl_core.h`, which two independent bindings also
//! carry: `windows`'s generated `IDesktopWallpaper_Impl`, and `WinDynamicDesktop`'s
//! `IDesktopWallpaper` `ComImport` interface, whose sixteen declarations end at
//! the same `Enable`.
//!
//! **What the research fixed.** `docs/research/windows.md` is the authority for
//! the method: `SetWallpaper(monitorID, path)` sets one monitor from the device
//! path `GetMonitorDevicePathAt` returns and `monitorID = NULL` sets every
//! monitor at once; `GetWallpaper` reads back, per monitor; the coclass is an
//! out-of-process shell server created with `CLSCTX_LOCAL_SERVER`; and a monitor
//! may still be reported after it is detached, so the device path and never the
//! index is the identity (docs/architecture.md 3.3).
//!
//! **Nothing here needs a window.** The call needs the user's interactive
//! session, not a UI process (docs/architecture.md 3.2 and 3.3, and
//! docs/research/windows.md 4: session 0 cannot set a wallpaper at all), and
//! `whirld` never links this file (docs/architecture.md R2: the daemon does not
//! depend on the worker's platform crate).
//!
//! **What this file deliberately does not do.** Position and style are not
//! touched: `SetPosition` is system-wide on this platform and not per monitor, so
//! per-monitor styling is not something this backend can offer and not something
//! it claims (docs/research/windows.md 2). The slideshow calls, the background
//! colour and `Enable` are separate capabilities the boundary does not expose, so
//! their slots are written down for the layout and have no call site. Virtual
//! desktops are absent because the interface does not model them at all
//! (docs/research/windows.md 1).

// The aliases below are the SDK's own names. `Hresult` and `Lpwstr` would read as
// this file being unsure which header it is calling, and every signature here is
// meant to be checkable against `shobjidl_core.h` line by line.
#![allow(clippy::upper_case_acronyms)]

use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::ptr;

use super::SetError;
use whirl_core::protocol::ErrorCode;

/// A COM `HRESULT`: `i32`, zero or positive for success, negative for failure.
type HRESULT = i32;
/// `LPCWSTR`: a NUL-terminated UTF-16 string, borrowed. `NULL` wherever the
/// interface documents `monitorID = NULL` as "every monitor".
type LPCWSTR = *const u16;
/// `LPWSTR`: a NUL-terminated UTF-16 string the platform allocates and the caller
/// frees with `CoTaskMemFree`.
type LPWSTR = *mut u16;

/// `S_OK`.
const S_OK: HRESULT = 0;
/// `COINIT_APARTMENTTHREADED`: the apartment the shell's wallpaper object wants,
/// and the one the working Rust binding in `docs/research/windows.md` 1 uses.
const COINIT_APARTMENTTHREADED: u32 = 0x2;
/// `CLSCTX_LOCAL_SERVER`: the coclass is out-of-process, which is what the
/// architecture records (docs/architecture.md 3.3) and what the research
/// observed (docs/research/windows.md 1).
const CLSCTX_LOCAL_SERVER: u32 = 0x4;

/// A COM `GUID`: the four fields in the order `guiddef.h` lays them out.
#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `CLSID_DesktopWallpaper`, `{C2CF3110-460E-4fc1-B9D0-8A1C0C9CC4BD}`, which
/// `WinDynamicDesktop`'s coclass carries as its `[Guid]`
/// (docs/research/windows.md 1: two independent implementations agree on both of
/// these, and the forum threads that disagree are not where they came from).
const CLSID_DESKTOP_WALLPAPER: Guid = Guid {
    data1: 0xC2CF_3110,
    data2: 0x460E,
    data3: 0x4FC1,
    data4: [0xB9, 0xD0, 0x8A, 0x1C, 0x0C, 0x9C, 0xC4, 0xBD],
};

/// `IID_IDesktopWallpaper`, `{B92B56A9-8B55-4E14-9A89-0199BBB6F93B}`, the
/// interface GUID `WinDynamicDesktop` and the `windows` crate both carry.
const IID_DESKTOP_WALLPAPER: Guid = Guid {
    data1: 0xB92B_56A9,
    data2: 0x8B55,
    data3: 0x4E14,
    data4: [0x9A, 0x89, 0x01, 0x99, 0xBB, 0xB6, 0xF9, 0x3B],
};

// `ole32` carries all four: `CoInitializeEx`, `CoUninitialize`,
// `CoCreateInstance` and `CoTaskMemFree` are one DLL's exports, so the linker is
// told the DLL once.
#[link(name = "ole32")]
unsafe extern "system" {
    /// `HRESULT CoInitializeEx(LPVOID pvReserved, DWORD dwCoInit)`.
    fn CoInitializeEx(pv_reserved: *mut c_void, coinit: u32) -> HRESULT;
    /// `void CoUninitialize(void)`.
    fn CoUninitialize();
    /// `HRESULT CoCreateInstance(REFCLSID rclsid, LPUNKNOWN pUnkOuter,
    /// DWORD dwClsContext, REFIID riid, LPVOID *ppv)`.
    fn CoCreateInstance(
        rclsid: *const Guid,
        outer: *mut c_void,
        clsctx: u32,
        riid: *const Guid,
        out: *mut *mut c_void,
    ) -> HRESULT;
    /// `void CoTaskMemFree(LPVOID pv)`.
    fn CoTaskMemFree(pv: *mut c_void);
}

/// `IDesktopWallpaper`'s vtable: `IUnknown`'s three slots, then the interface's
/// sixteen methods in the order `shobjidl_core.h` declares them.
///
/// The order is this type's whole contract, which is why the slots this file does
/// not call are written out instead of trimmed: a vtable is a layout, not a menu,
/// and a reader checking slot 5 against the header needs slot 5 in front of them.
/// The type is never constructed, only pointed at, so the unused slots cost
/// nothing in the binary; the `#[allow(dead_code)]` is for the compiler, which
/// cannot see the platform reading them.
#[repr(C)]
#[allow(dead_code)]
struct DesktopWallpaperVtbl {
    // IUnknown: slots 0, 1 and 2.
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    /// Slot 3: `HRESULT SetWallpaper(LPCWSTR monitorID, LPCWSTR wallpaper)`.
    set_wallpaper: unsafe extern "system" fn(*mut c_void, LPCWSTR, LPCWSTR) -> HRESULT,
    /// Slot 4: `HRESULT GetWallpaper(LPCWSTR monitorID, LPWSTR *wallpaper)`.
    get_wallpaper: unsafe extern "system" fn(*mut c_void, LPCWSTR, *mut LPWSTR) -> HRESULT,
    /// Slot 5: `HRESULT GetMonitorDevicePathAt(UINT monitorIndex,
    /// LPWSTR *monitorID)`.
    get_monitor_device_path_at: unsafe extern "system" fn(*mut c_void, u32, *mut LPWSTR) -> HRESULT,
    /// Slot 6: `HRESULT GetMonitorDevicePathCount(UINT *count)`.
    get_monitor_device_path_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    /// Slot 7: `HRESULT GetMonitorRECT(LPCWSTR monitorID, RECT *displayRect)`.
    get_monitor_rect: unsafe extern "system" fn(*mut c_void, LPCWSTR, *mut Rect) -> HRESULT,
    /// Slot 8: `HRESULT SetBackgroundColor(COLORREF color)`.
    set_background_color: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    /// Slot 9: `HRESULT GetBackgroundColor(COLORREF *color)`.
    get_background_color: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    /// Slot 10: `HRESULT SetPosition(DESKTOP_WALLPAPER_POSITION position)`.
    set_position: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    /// Slot 11: `HRESULT GetPosition(DESKTOP_WALLPAPER_POSITION *position)`.
    get_position: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    /// Slot 12: `HRESULT SetSlideshow(IShellItemArray *items)`.
    set_slideshow: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    /// Slot 13: `HRESULT GetSlideshow(IShellItemArray **items)`.
    get_slideshow: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    /// Slot 14: `HRESULT SetSlideshowOptions(DESKTOP_SLIDESHOW_OPTIONS options,
    /// UINT slideshowTick)`.
    set_slideshow_options: unsafe extern "system" fn(*mut c_void, i32, u32) -> HRESULT,
    /// Slot 15: `HRESULT GetSlideshowOptions(DESKTOP_SLIDESHOW_OPTIONS *options,
    /// UINT *slideshowTick)`.
    get_slideshow_options: unsafe extern "system" fn(*mut c_void, *mut i32, *mut u32) -> HRESULT,
    /// Slot 16: `HRESULT AdvanceSlideshow(LPCWSTR monitorID,
    /// DESKTOP_SLIDESHOW_DIRECTION direction)`.
    advance_slideshow: unsafe extern "system" fn(*mut c_void, LPCWSTR, i32) -> HRESULT,
    /// Slot 17: `HRESULT GetStatus(DESKTOP_SLIDESHOW_STATE *state)`.
    get_status: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
    /// Slot 18: `HRESULT Enable(BOOL enable)`.
    enable: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
}

/// `RECT`, the only struct the vtable mentions by value. Slots 7 to 18 are never
/// called from here, but the slot that holds this type is still written with it
/// so the layout is the header's.
#[repr(C)]
#[allow(dead_code)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// The `CoInitializeEx`/`CoUninitialize` pair for the calling thread.
///
/// `S_OK` and `S_FALSE` both mean COM is up on this thread, `S_FALSE` being "it
/// already was", and both are our call to pair, so both get the `CoUninitialize`
/// on drop. `RPC_E_CHANGED_MODE` is a failure and does not: the thread is in
/// another apartment and this thread's call never succeeded.
#[derive(Debug)]
struct Apartment(());

impl Apartment {
    /// `CoInitializeEx(NULL, COINIT_APARTMENTTHREADED)`.
    fn initialize(stage: Stage) -> Result<Apartment, SetError> {
        let hr = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        if !succeeded(hr) {
            return Err(refused(
                stage,
                hr,
                "COM could not be initialised for this thread",
            ));
        }
        Ok(Apartment(()))
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// A created `IDesktopWallpaper`, released on drop. Field 0 is the interface
/// pointer `CoCreateInstance` returned; the only part of it this file reads is the
/// vtable pointer at its first word, through [`DesktopWallpaper::vtable`].
#[derive(Debug)]
struct DesktopWallpaper(*mut c_void);

impl DesktopWallpaper {
    /// `CoCreateInstance(&CLSID_DesktopWallpaper, NULL, CLSCTX_LOCAL_SERVER,
    /// &IID_IDesktopWallpaper, &wallpaper)` for the interface this backend uses.
    fn create(stage: Stage) -> Result<DesktopWallpaper, SetError> {
        DesktopWallpaper::create_for(&CLSID_DESKTOP_WALLPAPER, &IID_DESKTOP_WALLPAPER, stage)
    }

    /// The same call with the class and interface named, so a refusal to create
    /// the interface can be exercised without a desktop (the test below asks for a
    /// class that is not registered).
    fn create_for(clsid: &Guid, iid: &Guid, stage: Stage) -> Result<DesktopWallpaper, SetError> {
        let mut raw: *mut c_void = ptr::null_mut();
        let hr =
            unsafe { CoCreateInstance(clsid, ptr::null_mut(), CLSCTX_LOCAL_SERVER, iid, &mut raw) };
        if !succeeded(hr) {
            return Err(refused(
                stage,
                hr,
                "the DesktopWallpaper COM object could not be created",
            ));
        }
        if raw.is_null() {
            return Err(refused(
                stage,
                S_OK,
                "CoCreateInstance answered S_OK and returned no interface pointer",
            ));
        }
        Ok(DesktopWallpaper(raw))
    }

    /// The vtable pointer: the first word of the object the interface pointer
    /// points at. Reading it is sound for the same reason every call below is: the
    /// pointer came from a successful `CoCreateInstance` for this IID, it is
    /// non-null, and `Drop` releases it exactly once.
    fn vtable(&self) -> *const DesktopWallpaperVtbl {
        unsafe { *(self.0 as *const *const DesktopWallpaperVtbl) }
    }

    /// `IDesktopWallpaper::SetWallpaper`, slot 3. `monitor` is `NULL` for the
    /// interface's documented "set the image on every monitor" call.
    fn set_wallpaper(&self, monitor: LPCWSTR, wallpaper: LPCWSTR) -> HRESULT {
        let call = unsafe { (*self.vtable()).set_wallpaper };
        unsafe { call(self.0, monitor, wallpaper) }
    }

    /// `IDesktopWallpaper::GetWallpaper`, slot 4, for one monitor. The platform
    /// allocates the string it writes, so it is freed here rather than handed back
    /// as a borrowed pointer: this is the one call that owns a buffer the caller
    /// did not allocate, and it must not be leaked.
    fn get_wallpaper(&self, monitor: LPCWSTR) -> Result<Option<String>, SetError> {
        let mut raw: LPWSTR = ptr::null_mut();
        let call = unsafe { (*self.vtable()).get_wallpaper };
        let hr = unsafe { call(self.0, monitor, &mut raw) };
        let value = take_wide_string(raw);
        wallpaper_answer(hr, value.as_deref())
    }

    /// `GetMonitorDevicePathCount` and then `GetMonitorDevicePathAt` for every
    /// index, so the answer is a list of monitor identities rather than a count
    /// (docs/architecture.md 3.3: the device path string is the identity, never
    /// the index).
    ///
    /// The list may be empty, which is a successful answer: a process with no
    /// interactive desktop has no monitors to name. What each caller does with it
    /// differs, and that is why the empty case is not decided here.
    fn monitors(&self, stage: Stage) -> Result<Vec<String>, SetError> {
        let mut count: u32 = 0;
        let call = unsafe { (*self.vtable()).get_monitor_device_path_count };
        let hr = unsafe { call(self.0, &mut count) };
        if !succeeded(hr) {
            return Err(refused(
                stage,
                hr,
                "GetMonitorDevicePathCount could not size the monitor list",
            ));
        }
        let mut monitors = Vec::new();
        for index in 0..count {
            let mut raw: LPWSTR = ptr::null_mut();
            let call = unsafe { (*self.vtable()).get_monitor_device_path_at };
            let hr = unsafe { call(self.0, index, &mut raw) };
            let monitor = take_wide_string(raw);
            if !succeeded(hr) {
                return Err(refused(
                    stage,
                    hr,
                    &format!("GetMonitorDevicePathAt({index}) could not name the monitor"),
                ));
            }
            match monitor {
                Some(monitor) => monitors.push(monitor),
                None => {
                    return Err(failure(
                        stage,
                        format!(
                            "GetMonitorDevicePathAt({index}) answered S_OK and named no monitor"
                        ),
                    ));
                }
            }
        }
        Ok(monitors)
    }
}

impl Drop for DesktopWallpaper {
    fn drop(&mut self) {
        let call = unsafe { (*self.vtable()).release };
        unsafe {
            let _ = call(self.0);
        }
    }
}

/// Which of the boundary's two calls is asking. It decides the code a refusal
/// carries, so the two are not told apart by reading a message.
#[derive(Clone, Copy)]
enum Stage {
    Set,
    Readback,
}

impl Stage {
    /// The `ErrorCode` this stage's refusals carry: `set_failed` for the setter
    /// and `internal` for the readback, both from the closed set of
    /// docs/architecture.md 2.7. A readback that cannot be taken refuses nothing,
    /// which is why it is not `set_failed`.
    fn code(self) -> ErrorCode {
        match self {
            Stage::Set => ErrorCode::SetFailed,
            Stage::Readback => ErrorCode::Internal,
        }
    }
}

/// The refusal a stage carries, for a failure with no `HRESULT` of its own.
fn failure(stage: Stage, reason: impl Into<String>) -> SetError {
    SetError::new(stage.code(), reason)
}

/// The `HRESULT`-to-`ErrorCode` mapping, in one place: the stage names the code
/// and the message carries the documented symbol and its number, so the line
/// someone pastes into a report is one they can look up.
fn refused(stage: Stage, hr: HRESULT, what: &str) -> SetError {
    failure(stage, format!("{what}: {}", describe_hr(hr)))
}

/// `SUCCEEDED`: everything at or above zero, `S_FALSE` included. Reading
/// `S_FALSE` as a failure is the classic `HRESULT` mistake, and on the readback it
/// is the difference between "the platform answered" and "the platform could not
/// be asked".
fn succeeded(hr: HRESULT) -> bool {
    hr >= 0
}

/// The documented symbol for every `HRESULT` these calls can return, and `None`
/// for one this file cannot name. The numbers are `winerror.h`'s.
fn hr_name(hr: HRESULT) -> Option<&'static str> {
    match hr as u32 {
        0x0000_0000 => Some("S_OK"),
        0x0000_0001 => Some("S_FALSE"),
        0x8000_4001 => Some("E_NOTIMPL"),
        0x8000_4002 => Some("E_NOINTERFACE"),
        0x8000_4003 => Some("E_POINTER"),
        0x8000_4004 => Some("E_ABORT"),
        0x8000_4005 => Some("E_FAIL"),
        0x8000_FFFF => Some("E_UNEXPECTED"),
        0x8004_0110 => Some("CLASS_E_NOAGGREGATION"),
        0x8004_0111 => Some("CLASS_E_CLASSNOTAVAILABLE"),
        0x8004_0154 => Some("REGDB_E_CLASSNOTREG"),
        0x8004_0155 => Some("REGDB_E_IIDNOTREG"),
        0x8004_01F0 => Some("CO_E_NOTINITIALIZED"),
        0x8004_01F1 => Some("CO_E_ALREADYINITIALIZED"),
        0x8001_0106 => Some("RPC_E_CHANGED_MODE"),
        0x8001_010E => Some("RPC_E_WRONG_THREAD"),
        0x8008_0005 => Some("CO_E_SERVER_EXEC_FAILURE"),
        0x8007_0002 => Some("E_FILE_NOT_FOUND"),
        0x8007_0003 => Some("E_PATH_NOT_FOUND"),
        0x8007_0005 => Some("E_ACCESSDENIED"),
        0x8007_0006 => Some("E_HANDLE"),
        0x8007_000E => Some("E_OUTOFMEMORY"),
        0x8007_0057 => Some("E_INVALIDARG"),
        0x8007_05B3 => Some("ERROR_REQUIRES_INTERACTIVE_WINDOWSTATION"),
        _ => None,
    }
}

/// What the number means to the person who will paste it into a report, for the
/// `HRESULT`s where it changes what they do next: the two documented ways this API
/// is out of reach are session 0 and the logon race (docs/research/windows.md 4),
/// and a flat symbol would not say which one happened.
fn hr_note(hr: HRESULT) -> Option<&'static str> {
    match hr as u32 {
        0x8007_05B3 => Some(
            "this process has no interactive window station (Win32 error 1459), \
             which is what a service or a session-0 task looks like; a wallpaper can \
             only be set from the user's own interactive session",
        ),
        0x8004_0154 => Some(
            "the shell's DesktopWallpaper coclass is not registered in this session, \
             which is what the shell still coming up at logon looks like",
        ),
        0x8001_0106 => Some(
            "this thread is already in a different COM apartment, so the shell's \
             object cannot be reached from it",
        ),
        0x8008_0005 => Some("the out-of-process COM server could not be started"),
        0x8007_0005 => Some(
            "access was denied, which is what changing another principal's \
             per-user setting looks like",
        ),
        _ => None,
    }
}

/// A failed call as the sentence a person reads: the documented symbol, the
/// number, and — where it decides what to do next — which way it failed.
fn describe_hr(hr: HRESULT) -> String {
    let number = hr as u32;
    match (hr_name(hr), hr_note(hr)) {
        (Some(name), Some(note)) => format!("{name} (0x{number:08X}): {note}"),
        (Some(name), None) => format!("{name} (0x{number:08X})"),
        (None, Some(note)) => format!("HRESULT 0x{number:08X}: {note}"),
        (None, None) => format!("HRESULT 0x{number:08X}"),
    }
}

/// A Rust string as the NUL-terminated UTF-16 buffer every `LPCWSTR` here needs.
/// `None` when the value holds a NUL byte: `LPCWSTR` stops at the first one, so
/// handing it over would name a different string than the caller asked for, and
/// `encode_wide` cannot produce a NUL except from one in its input.
fn to_wide(value: &str) -> Option<Vec<u16>> {
    if value.contains('\0') {
        return None;
    }
    let mut wide: Vec<u16> = OsStr::new(value).encode_wide().collect();
    wide.push(0);
    Some(wide)
}

/// The Rust string behind an `LPWSTR` the platform allocated, and the free that
/// goes with it: `GetWallpaper` and `GetMonitorDevicePathAt` both hand back a
/// buffer the caller owns and frees with `CoTaskMemFree`.
///
/// `None` for a null pointer and for the empty string, because the platform's own
/// documentation treats the empty string as "no string" for both methods: an
/// empty wallpaper path is "no image here", and an empty monitor name is no name.
fn take_wide_string(raw: LPWSTR) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    let mut length = 0usize;
    while unsafe { *raw.add(length) } != 0 {
        length += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(raw, length) };
    let value = String::from_utf16_lossy(slice);
    unsafe { CoTaskMemFree(raw as *mut c_void) };
    if value.is_empty() { None } else { Some(value) }
}

/// What one `GetWallpaper` answer means: the `HRESULT` the call returned and the
/// string it wrote through the out-parameter.
///
/// `GetWallpaper`'s own documentation says the out-parameter is empty both when
/// there is no image and when the call fails, so the empty string is only "no
/// image" *after a success*. A failed call is `Err` however empty it looks,
/// because `Ok(None)` is a verified absence and an unverified anchor is a
/// different thing (docs/architecture.md 1.7.3 step 3).
fn wallpaper_answer(hr: HRESULT, value: Option<&str>) -> Result<Option<String>, SetError> {
    if !succeeded(hr) {
        return Err(refused(
            Stage::Readback,
            hr,
            "the platform would not report the image on the desktop",
        ));
    }
    match value {
        Some(value) if !value.is_empty() => Ok(Some(value.to_string())),
        _ => Ok(None),
    }
}

/// The one image the whole desktop is showing, from one answer per monitor.
///
/// `Ok(None)` is the platform reporting no image on any of them, which is a
/// verified answer. Monitors showing different images are `Err`: there is no
/// single path to report, and naming one of them would claim the desktop is
/// something it is not (docs/architecture.md 1.7.3 step 2 wants a path the
/// display either matches or does not).
fn anchor_from(answers: &[(String, Option<String>)]) -> Result<Option<String>, String> {
    let mut anchor: Option<&str> = None;
    let mut differing: Vec<&str> = Vec::new();
    for (_monitor, image) in answers {
        let Some(image) = image.as_deref() else {
            continue;
        };
        match anchor {
            None => anchor = Some(image),
            Some(first) if first == image => {}
            Some(first) => {
                for image in [first, image] {
                    if !differing.contains(&image) {
                        differing.push(image);
                    }
                }
            }
        }
    }
    match anchor {
        None => Ok(None),
        Some(image) if differing.is_empty() => Ok(Some(image.to_string())),
        Some(_) => Err(format!(
            "the monitors are showing different images ({}), so there is no single \
             path to report as the desktop's",
            differing.join(", "),
        )),
    }
}

/// The monitor argument every `SetWallpaper` call takes, in order: one per device
/// path the platform enumerated, or the single `None` — the interface's `NULL`
/// monitor form — when it enumerated none.
///
/// `None` is not a fallback for a failure: it is the documented "set one image on
/// every monitor" call, and it is the one of the two forms that needs no
/// enumeration at all.
fn set_arguments(monitors: &[String]) -> Vec<Option<&str>> {
    if monitors.is_empty() {
        vec![None]
    } else {
        monitors
            .iter()
            .map(|monitor| Some(monitor.as_str()))
            .collect()
    }
}

/// The target of one `SetWallpaper` call, as a refusal names it.
fn describe_target(target: Option<&str>) -> String {
    match target {
        Some(monitor) => format!("SetWallpaper on the monitor {monitor}"),
        None => "SetWallpaper on all monitors (the NULL monitor form)".to_string(),
    }
}

/// Put `path` on the desktop of every monitor.
///
/// Enumerate first, then one call per device path: the path string and not the
/// index is a monitor's identity, and a monitor that vanished between the
/// enumeration and its call fails that call rather than shifting another
/// (docs/architecture.md 3.3). When there is nothing to enumerate, the single
/// `NULL` call is the documented all-monitors form.
///
/// A failed call is `set_failed` and the loop stops there, the same way the macOS
/// setter fails on the first screen that refuses: the caller asked for one image
/// everywhere and must not be told it got it. This is the one call behind the
/// backend boundary (docs/development.md section 7) and the only place the noop
/// and native paths differ.
pub fn set(path: &str) -> Result<(), SetError> {
    let wide_path = to_wide(path).ok_or_else(|| {
        failure(
            Stage::Set,
            format!(
                "the Windows setter cannot set {path:?}: the path holds a NUL byte, and \
                 LPCWSTR stops at it, so a different file would be named"
            ),
        )
    })?;

    let _apartment = Apartment::initialize(Stage::Set)?;
    let wallpaper = DesktopWallpaper::create(Stage::Set)?;
    let monitors = wallpaper.monitors(Stage::Set)?;

    for target in set_arguments(&monitors) {
        let monitor = match target {
            Some(monitor) => Some(to_wide(monitor).ok_or_else(|| {
                failure(
                    Stage::Set,
                    format!(
                        "the Windows setter cannot address the monitor {monitor:?}: its \
                         device path holds a NUL byte"
                    ),
                )
            })?),
            None => None,
        };
        let monitor = monitor.as_deref().map_or(ptr::null(), |wide| wide.as_ptr());
        let hr = wallpaper.set_wallpaper(monitor, wide_path.as_ptr());
        if !succeeded(hr) {
            return Err(refused(Stage::Set, hr, &describe_target(target)));
        }
    }
    Ok(())
}

/// What the platform reports on the desktop, read with one `GetWallpaper` call
/// per monitor (docs/architecture.md 1.7.3 step 1, and docs/research/windows.md 1:
/// the single `NULL` read collapses to `S_FALSE` with an empty string as soon as
/// the monitors disagree, so the per-monitor read is the one that answers).
///
/// `Ok(None)` is the platform reporting no image on any monitor, which is a
/// verified answer. Everything else is `Err`: a monitor it would not read, or
/// monitors showing different images, where there is no single path to report.
/// `Err` and never `Ok(None)` for both, because an unverified anchor is a
/// different thing from a verified absence (docs/architecture.md 1.7.3 step 3).
pub fn current() -> Result<Option<String>, SetError> {
    let _apartment = Apartment::initialize(Stage::Readback)?;
    let wallpaper = DesktopWallpaper::create(Stage::Readback)?;
    let monitors = wallpaper.monitors(Stage::Readback)?;
    if monitors.is_empty() {
        return Err(failure(
            Stage::Readback,
            "the platform enumerated no monitors, so this process has no interactive \
             desktop and the image on the desktop cannot be read back",
        ));
    }

    let mut answers = Vec::new();
    for monitor in &monitors {
        let wide = to_wide(monitor).ok_or_else(|| {
            failure(
                Stage::Readback,
                format!(
                    "the monitor {monitor:?} cannot be addressed: its device path holds a NUL byte"
                ),
            )
        })?;
        answers.push((monitor.clone(), wallpaper.get_wallpaper(wide.as_ptr())?));
    }
    anchor_from(&answers).map_err(|reason| failure(Stage::Readback, reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The HRESULTs a Windows machine can answer these calls with. Declared here
    /// rather than in the module because the code above compares only `S_OK` and
    /// the sign, and a named constant nothing reads is a warning.
    const S_FALSE: HRESULT = 0x0000_0001;
    const E_FAIL: HRESULT = 0x8000_4005u32 as i32;
    const E_ACCESSDENIED: HRESULT = 0x8007_0005u32 as i32;
    const E_INVALIDARG: HRESULT = 0x8007_0057u32 as i32;
    const RPC_E_CHANGED_MODE: HRESULT = 0x8001_0106u32 as i32;
    const REGDB_E_CLASSNOTREG: HRESULT = 0x8004_0154u32 as i32;
    const CO_E_SERVER_EXEC_FAILURE: HRESULT = 0x8008_0005u32 as i32;
    const ERROR_REQUIRES_INTERACTIVE_WINDOWSTATION: HRESULT = 0x8007_05B3u32 as i32;
    /// `COINIT_MULTITHREADED`, the other apartment, for the test that puts a
    /// thread in it before asking for an STA.
    const COINIT_MULTITHREADED: u32 = 0x0;
    /// A coclass that is not registered on any machine, so `CoCreateInstance`
    /// refuses it wherever this runs.
    const NOT_REGISTERED: Guid = Guid {
        data1: 0xDEAD_BEEF,
        data2: 0xC0DE,
        data3: 0x4A5E,
        data4: [0xA5, 0x5A, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01],
    };

    /// `SUCCEEDED` is `>= 0`, so `S_FALSE` is the platform answering. Pins the
    /// helper every other decision here rests on.
    #[test]
    fn s_false_is_the_platform_answering_and_not_a_failure() {
        assert!(succeeded(S_OK));
        assert!(succeeded(S_FALSE));
        assert!(!succeeded(E_FAIL));
        assert!(!succeeded(E_ACCESSDENIED));
    }

    /// `Err` when the platform could not be asked, never `Ok(None)`: the docs say
    /// a failed `GetWallpaper` leaves the out-parameter empty too, so a path in it
    /// is not evidence the call succeeded.
    #[test]
    fn a_failed_readback_is_an_error_and_never_a_verified_absence() {
        for hr in [E_FAIL, E_ACCESSDENIED, E_INVALIDARG] {
            let error = wallpaper_answer(hr, None).expect_err("a failed read is not an answer");
            assert_eq!(error.code, ErrorCode::Internal);
            let name = hr_name(hr).expect("a named HRESULT");
            assert!(error.message.contains(name), "{}", error.message);
        }
        let error = wallpaper_answer(E_FAIL, Some(r"C:\wallpapers\one.jpg"))
            .expect_err("a failed read is not an answer, path in the buffer or not");
        assert_eq!(error.code, ErrorCode::Internal);
    }

    /// A successful read whose string is empty is the platform's own "no image
    /// here"; `S_OK` and `S_FALSE` are both successes.
    #[test]
    fn an_empty_string_after_a_successful_readback_is_no_image() {
        assert_eq!(wallpaper_answer(S_OK, None).expect("a success"), None);
        assert_eq!(wallpaper_answer(S_OK, Some("")).expect("a success"), None);
        assert_eq!(
            wallpaper_answer(S_FALSE, Some("")).expect("a success"),
            None
        );
        assert_eq!(
            wallpaper_answer(S_OK, Some(r"C:\wallpapers\one.jpg")).expect("a success"),
            Some(r"C:\wallpapers\one.jpg".to_string())
        );
    }

    /// The `HRESULT`-to-`ErrorCode` mapping: the same platform failure is
    /// `set_failed` where a set refused and `internal` where a readback could not
    /// be taken (docs/architecture.md 2.7, and a readback refuses nothing).
    #[test]
    fn the_error_code_follows_the_stage_and_the_message_names_the_hresult() {
        let setting = refused(
            Stage::Set,
            E_ACCESSDENIED,
            "SetWallpaper on the monitor one",
        );
        assert_eq!(setting.code, ErrorCode::SetFailed);
        assert!(
            setting.message.contains("SetWallpaper on the monitor one"),
            "{}",
            setting.message
        );

        let reading = refused(
            Stage::Readback,
            E_ACCESSDENIED,
            "the platform would not report",
        );
        assert_eq!(reading.code, ErrorCode::Internal);

        for error in [&setting, &reading] {
            assert!(
                error.message.contains("E_ACCESSDENIED"),
                "{}",
                error.message
            );
            assert!(error.message.contains("0x80070005"), "{}", error.message);
        }
    }

    /// Each refusal says which one happened: no interactive desktop, a coclass the
    /// session has not registered, an apartment this thread cannot use. An
    /// `HRESULT` this file has never seen is still reported with its number.
    #[test]
    fn every_refusal_names_the_way_it_failed() {
        let no_desktop = describe_hr(ERROR_REQUIRES_INTERACTIVE_WINDOWSTATION);
        assert!(no_desktop.contains("0x800705B3"), "{no_desktop}");
        assert!(no_desktop.contains("1459"), "{no_desktop}");
        assert!(no_desktop.contains("interactive"), "{no_desktop}");

        let not_registered = describe_hr(REGDB_E_CLASSNOTREG);
        assert!(
            not_registered.contains("not registered"),
            "{not_registered}"
        );

        let apartment = describe_hr(RPC_E_CHANGED_MODE);
        assert!(apartment.contains("apartment"), "{apartment}");

        let server = describe_hr(CO_E_SERVER_EXEC_FAILURE);
        assert!(server.contains("COM server"), "{server}");

        let unknown = describe_hr(0x1234_5678u32 as i32);
        assert!(unknown.contains("0x12345678"), "{unknown}");
        assert!(unknown.contains("HRESULT"), "{unknown}");
        assert!(!unknown.contains("not implemented"), "{unknown}");
    }

    /// The monitor-id argument handling: one call per device path in the
    /// platform's enumeration order, or the single `NULL` form when it enumerated
    /// none. The index is never the argument (docs/architecture.md 3.3).
    #[test]
    fn the_monitor_argument_is_each_device_path_or_the_null_form() {
        assert_eq!(set_arguments(&[]), vec![None]);
        let monitors = ["monitor-one".to_string(), "monitor-two".to_string()];
        assert_eq!(
            set_arguments(&monitors),
            vec![Some("monitor-one"), Some("monitor-two"),]
        );
    }

    /// The refusal that never reaches the platform: `LPCWSTR` stops at the first
    /// NUL, so a path holding one would name a different file. Decided before COM
    /// is touched, which also makes it the one refusal testable anywhere.
    #[test]
    fn a_path_holding_a_nul_is_refused_before_any_call() {
        let wide = to_wide(r"C:\wallpapers\one.jpg").expect("a path without a NUL");
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(wide[0], u16::from(b'C'));
        assert_eq!(wide.len(), r"C:\wallpapers\one.jpg".len() + 1);
        assert_eq!(to_wide("one\0two"), None);

        let error = set("one\0two").expect_err("a path with a NUL is not set");
        assert_eq!(error.code, ErrorCode::SetFailed);
        assert!(error.message.contains("NUL"), "{}", error.message);
    }

    /// The refusal path when the interface cannot be created, run for real: a
    /// coclass that is not registered cannot be turned into the interface, and
    /// what comes back is `set_failed` with the platform's `HRESULT` in it.
    #[test]
    fn an_interface_that_cannot_be_created_is_refused_before_anything_is_set() {
        let _apartment = Apartment::initialize(Stage::Set).expect("COM initialises on Windows");
        let error =
            DesktopWallpaper::create_for(&NOT_REGISTERED, &IID_DESKTOP_WALLPAPER, Stage::Set)
                .expect_err("a coclass that is not registered cannot be created");
        assert_eq!(error.code, ErrorCode::SetFailed);
        assert!(
            error
                .message
                .contains("DesktopWallpaper COM object could not be created"),
            "{}",
            error.message
        );
        assert!(error.message.contains("0x"), "{}", error.message);
        assert!(
            !error.message.contains("not implemented"),
            "{}",
            error.message
        );
        println!("CoCreateInstance(NOT_REGISTERED) -> {}", error.message);
    }

    /// The other refusal path: COM cannot be initialised for this thread. A thread
    /// already in a different apartment answers `RPC_E_CHANGED_MODE`, and the
    /// message says which of the two that was rather than blaming the machine.
    #[test]
    fn a_thread_in_another_apartment_is_refused_and_the_message_says_which() {
        let hr = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED) };
        assert!(
            succeeded(hr),
            "the thread takes an MTA: {}",
            describe_hr(hr)
        );

        let error =
            Apartment::initialize(Stage::Set).expect_err("an STA on an MTA thread is refused");
        assert_eq!(error.code, ErrorCode::SetFailed);
        assert!(
            error.message.contains("RPC_E_CHANGED_MODE"),
            "{}",
            error.message
        );
        assert!(error.message.contains("apartment"), "{}", error.message);

        let readback = Apartment::initialize(Stage::Readback)
            .expect_err("an STA on an MTA thread is refused for the readback too");
        assert_eq!(readback.code, ErrorCode::Internal);

        unsafe { CoUninitialize() };
    }

    /// A desktop whose monitors disagree has no single anchor: reporting one of
    /// them would be an unverified anchor. Monitors that agree, one image and one
    /// blank, and all blank are the three answers that do have one.
    #[test]
    fn a_desktop_of_disagreeing_monitors_has_no_anchor_to_report() {
        let one = r"C:\wallpapers\one.jpg".to_string();
        let two = r"C:\wallpapers\two.jpg".to_string();
        let monitor =
            |name: &str, image: Option<&str>| (name.to_string(), image.map(str::to_string));

        assert_eq!(
            anchor_from(&[monitor("one", Some(&one)), monitor("two", None)]).expect("an answer"),
            Some(one.clone())
        );
        assert_eq!(
            anchor_from(&[monitor("one", Some(&one)), monitor("two", Some(&one))])
                .expect("an answer"),
            Some(one.clone())
        );
        assert_eq!(
            anchor_from(&[monitor("one", None), monitor("two", None)]).expect("an answer"),
            None
        );

        let reason = anchor_from(&[monitor("one", Some(&one)), monitor("two", Some(&two))])
            .expect_err("two images are not one anchor");
        assert!(reason.contains(&one), "{reason}");
        assert!(reason.contains(&two), "{reason}");
        assert!(reason.contains("different"), "{reason}");
    }
}
