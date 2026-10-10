//! AppleScript: the commands in `resources/CleanShotZ.sdef` all arrive at
//! one command class, which turns them into the same requests as
//! `cleanshotz://` URLs and the command line.
//!
//! ```applescript
//! tell application "CleanShotZ"
//!     capture area x 0 y 0 width 800 height 600 action "pin"
//!     capture text from file (POSIX file "/tmp/receipt.png") without line breaks
//! end tell
//! ```

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, define_class};
use objc2_foundation::{NSNumber, NSScriptCommand, NSString, NSURL};

/// The AppleScript error for a request that could not run.
const ERROR: isize = -2700;

define_class!(
    // SAFETY: NSScriptCommand is meant to be subclassed for custom
    // commands; no Drop implementation. Cocoa scripting runs commands on
    // the main thread.
    #[unsafe(super(NSScriptCommand))]
    #[name = "CSZScriptCommand"]
    pub struct ScriptCommand;

    impl ScriptCommand {
        #[unsafe(method_id(performDefaultImplementation))]
        fn perform(&self) -> Option<Retained<AnyObject>> {
            if let Err(e) = self.run() {
                tracing::warn!(error = %e, "AppleScript command failed");
                self.setScriptErrorNumber(ERROR);
                self.setScriptErrorString(Some(&NSString::from_str(&e)));
            }
            None
        }
    }
);

/// Registers the command class, so Cocoa scripting finds it by name when
/// the first Apple event arrives.
pub fn register() {
    let _ = ScriptCommand::class();
}

impl ScriptCommand {
    fn run(&self) -> Result<(), String> {
        // "capture area" is the URL command "capture-area".
        let name = self
            .commandDescription()
            .commandName()
            .to_string()
            .replace(' ', "-");
        let mut params = Vec::new();
        if let Some(args) = self.evaluatedArguments() {
            for key in args.allKeys().iter() {
                let Some(value) = args.objectForKey(&key) else {
                    continue;
                };
                params.push((key.to_string(), parameter_value(&value)?));
            }
        }
        let url = crate::command::to_url(&name, &params);
        super::server::handle_url(&url)
    }
}

/// AppleScript values as URL parameter text: numbers, booleans, text and
/// files (as POSIX paths).
fn parameter_value(value: &AnyObject) -> Result<String, String> {
    if let Some(url) = value.downcast_ref::<NSURL>() {
        return url
            .path()
            .map(|p| p.to_string())
            .ok_or_else(|| "the file has no path".to_string());
    }
    if let Some(text) = value.downcast_ref::<NSString>() {
        return Ok(text.to_string());
    }
    if let Some(number) = value.downcast_ref::<NSNumber>() {
        // Booleans arrive as NSNumber too; their type encoding is 'c'.
        let encoding = number.objCType();
        // SAFETY: objCType returns a valid C string owned by the number.
        let kind = unsafe { std::ffi::CStr::from_ptr(encoding.as_ptr()) };
        return Ok(if kind.to_bytes() == b"c" || kind.to_bytes() == b"B" {
            number.boolValue().to_string()
        } else {
            let n = number.doubleValue();
            if n.fract() == 0.0 {
                format!("{n:.0}")
            } else {
                format!("{n}")
            }
        });
    }
    Err("a parameter has a value of an unexpected kind".into())
}

/// Development aid (debug builds): runs AppleScript `source` in the app.
#[cfg(debug_assertions)]
pub fn run_source(source: &str) -> Result<(), String> {
    use objc2::AllocAnyThread;
    use objc2_foundation::NSAppleScript;
    let script = NSAppleScript::initWithSource(NSAppleScript::alloc(), &NSString::from_str(source))
        .ok_or("could not create the script")?;
    let mut error: *mut objc2_foundation::NSDictionary = std::ptr::null_mut();
    // SAFETY: a valid script; `error` receives an autoreleased dictionary
    // on failure, when the result is nil (which the generated binding does
    // not allow for, hence msg_send).
    let result: Option<Retained<objc2_foundation::NSAppleEventDescriptor>> =
        unsafe { objc2::msg_send![&script, executeAndReturnError: &mut error] };
    // SAFETY: nil or a valid dictionary, alive for this autorelease pool.
    let error = unsafe { error.as_ref() };
    tracing::info!(?result, ?error, "dev AppleScript ran");
    match error {
        Some(e) => {
            let message = e
                .objectForKey(&NSString::from_str("NSAppleScriptErrorMessage"))
                .and_then(|m| m.downcast::<NSString>().ok())
                .map_or_else(|| format!("{e:?}"), |m| m.to_string());
            Err(message)
        }
        None => Ok(()),
    }
}
