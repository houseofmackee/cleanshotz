//! Screen recording.

pub mod engine;
pub mod keys;
pub mod media;
pub mod overlays;
mod session;

pub use session::{is_recording, toggle};

/// Called on the main thread when the system stops a recording (for
/// example from the screen sharing menu).
pub fn stream_stopped_by_system() {
    session::stream_stopped_by_system();
}

/// Development aid (debug builds): records part of the main display for
/// `seconds`, pausing for one second in the middle, and writes `output`.
#[cfg(debug_assertions)]
pub fn record_sample(output: &std::path::Path, seconds: f64, audio: bool) -> Result<(), String> {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::capture::geometry::Rect;
    use crate::capture::sck::{self, Content};

    let content = Content::fetch().map_err(|e| e.to_string())?;
    let display_id = objc2_core_graphics::CGMainDisplayID();
    let display = content.display(display_id).ok_or("no main display")?;
    let filter = sck::recording_filter(&content, &display);
    let scale = sck::pixel_scale(&filter);
    let rect = Rect::new(100.0, 100.0, 800.0, 500.0);
    let options = engine::Options {
        filter,
        source_rect: Some(rect),
        width: (rect.width * scale) as usize,
        height: (rect.height * scale) as usize,
        fps: 30,
        codec: crate::config::Codec::H264,
        bitrate: 8_000_000,
        show_cursor: true,
        show_clicks: true,
        ignore_shadows: false,
        system_audio: audio,
        microphone: false,
        output: output.to_path_buf(),
    };
    let recorder = engine::Recorder::start(options)?;
    let run_for = |d: Duration| {
        let until = Instant::now() + d;
        while Instant::now() < until {
            objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(
                &objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05),
            );
        }
    };
    let half = Duration::from_secs_f64(seconds / 2.0);
    run_for(half);
    recorder.set_paused(true);
    run_for(Duration::from_secs(1));
    recorder.set_paused(false);
    run_for(half);
    let result: Arc<Mutex<Option<Result<std::path::PathBuf, String>>>> = Arc::default();
    let slot = result.clone();
    recorder.stop(move |r| *slot.lock().unwrap() = Some(r));
    let deadline = Instant::now() + Duration::from_secs(20);
    while result.lock().unwrap().is_none() && Instant::now() < deadline {
        objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(
            &objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05),
        );
    }
    let r = result.lock().unwrap().take().ok_or("stopping timed out")?;
    r.map(|p| println!("recorded {}", p.display()))
}
