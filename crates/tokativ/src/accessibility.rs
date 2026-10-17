//! The Accessibility permission macOS wants before Tokativ may type or paste into other apps.
//! macOS ties the permission to the app's code signature. A build signed another way, such as an
//! ad-hoc build replaced by a signed one, keeps its switch on in System Settings while the grant
//! no longer applies, and text silently stops arriving.

use std::ffi::c_void;
use std::fs;
use std::process::Command;

/// The bundle identifier the permission is filed under.
const BUNDLE: &str = "io.github.nolight132.tokativ";
/// The file in the config folder that records the version the stale grant was last cleared for.
const MARKER: &str = "accessibility-reset";

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// Asks for the permission again when an install that was set up has lost it, which happens after
/// an update changes the signature. The old entry is cleared first, since macOS will not prompt
/// while it is there. This runs once per version, so a declined prompt does not come back on
/// every launch. A first run is left alone and asks when it first types.
pub fn recover(setup_done: bool) {
    // SAFETY: a plain query of the calling process's own trust.
    if !setup_done || unsafe { AXIsProcessTrusted() } {
        return;
    }
    let marker = tokativ_config::config_dir().join(MARKER);
    let version = env!("CARGO_PKG_VERSION");
    if fs::read_to_string(&marker).is_ok_and(|done| done.trim() == version) {
        return;
    }
    log::warn!("accessibility: not granted to this build, asking again");
    match Command::new("/usr/bin/tccutil")
        .args(["reset", "Accessibility", BUNDLE])
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => log::warn!("accessibility: tccutil exited with {status}"),
        Err(error) => log::warn!("accessibility: cannot run tccutil: {error}"),
    }
    if let Err(error) = fs::write(&marker, version) {
        log::warn!("accessibility: cannot write {}: {error}", marker.display());
    }
    prompt();
}

/// Shows the system dialog that offers to open the Accessibility settings.
fn prompt() {
    // SAFETY: the keys and values are CoreFoundation constants that live for the whole process,
    // the callbacks are the standard CFType ones, and the dictionary is released after use.
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        );
        if options.is_null() {
            return;
        }
        AXIsProcessTrustedWithOptions(options);
        CFRelease(options);
    }
}
