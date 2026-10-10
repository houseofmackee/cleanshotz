//! The recording engine: a ScreenCaptureKit stream feeding an
//! `AVAssetWriter`.
//!
//! Sample buffers arrive on a private serial queue, which is also the only
//! place the writer is touched. Pausing drops samples and shifts later
//! timestamps back by the paused time, so the file has no gap.

use std::path::PathBuf;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVCaptureAudioDataOutput,
    AVCaptureAudioDataOutputSampleBufferDelegate, AVCaptureConnection, AVCaptureDevice,
    AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession, AVFileTypeMPEG4, AVMediaTypeAudio,
    AVMediaTypeVideo, AVVideoAverageBitRateKey, AVVideoCodecKey, AVVideoCodecTypeH264,
    AVVideoCodecTypeHEVC, AVVideoCompressionPropertiesKey, AVVideoExpectedSourceFrameRateKey,
    AVVideoHeightKey, AVVideoMaxKeyFrameIntervalKey, AVVideoWidthKey,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_media::{CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags};
use objc2_foundation::{NSDictionary, NSError, NSNumber, NSObjectProtocol, NSString, NSURL};
use objc2_screen_capture_kit::{
    SCContentFilter, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
    SCStreamOutputType,
};

use crate::capture::geometry::Rect;
use crate::config::Codec;

pub struct Options {
    pub filter: Retained<SCContentFilter>,
    /// Region in display-local points; `None` records the whole filter.
    pub source_rect: Option<Rect>,
    /// Output size in pixels (even numbers).
    pub width: usize,
    pub height: usize,
    pub fps: u32,
    pub codec: Codec,
    pub bitrate: u64,
    pub show_cursor: bool,
    pub show_clicks: bool,
    /// Window recordings: record the window without its shadow, so the
    /// content is not scaled to make room for it.
    pub ignore_shadows: bool,
    pub system_audio: bool,
    pub microphone: bool,
    pub output: PathBuf,
}

/// Wraps Objective-C objects that are only used under a mutex or on the
/// recording queue.
struct AssertSend<T>(T);
// SAFETY: see the type's documentation.
unsafe impl<T> Send for AssertSend<T> {}

impl<T> AssertSend<T> {
    /// Taking `self` makes closures capture the whole wrapper rather than
    /// just its (non-Send) field.
    fn into_inner(self) -> T {
        self.0
    }
}

struct WriterState {
    writer: Retained<AVAssetWriter>,
    video: Retained<AVAssetWriterInput>,
    audio: Option<Retained<AVAssetWriterInput>>,
    microphone: Option<Retained<AVAssetWriterInput>>,
    started: bool,
    paused: bool,
    /// Timestamp of the first sample dropped in the current pause.
    pause_start: Option<CMTime>,
    /// Total paused time, subtracted from later timestamps.
    offset: CMTime,
    last_video: Option<CMTime>,
    finishing: bool,
    error: Option<String>,
}

// SAFETY: the Objective-C objects are only used while holding the mutex,
// from the recording queue.
unsafe impl Send for WriterState {}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[ivars = Mutex<WriterState>]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            self.handle(buffer, kind);
        }
    }

    unsafe impl SCStreamDelegate for StreamOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            let message = error.localizedDescription().to_string();
            tracing::warn!(error = %message, "recording stream stopped");
            if let Ok(mut state) = self.ivars().lock() {
                state.error.get_or_insert(message);
            }
            // The user may have stopped sharing from the system UI.
            DispatchQueue::main().exec_async(super::stream_stopped_by_system);
        }
    }
);

impl StreamOutput {
    fn handle(&self, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
        // SAFETY: plain sample buffer queries.
        if !unsafe { buffer.is_valid() } {
            return;
        }
        let Ok(mut state) = self.ivars().lock() else {
            return;
        };
        if state.finishing || state.error.is_some() {
            return;
        }
        // SAFETY: plain sample buffer query.
        let pts = unsafe { buffer.presentation_time_stamp() };

        if kind == SCStreamOutputType::Screen {
            // Idle frames (nothing changed on screen) carry no image.
            // SAFETY: plain sample buffer query.
            if unsafe { buffer.image_buffer() }.is_none() {
                return;
            }
            if !state.started {
                // SAFETY: the writer is in the writing state.
                unsafe { state.writer.startSessionAtSourceTime(pts) };
                state.started = true;
            }
        } else if !state.started {
            // Audio before the first frame would start the file with silence.
            return;
        }

        if state.paused {
            if state.pause_start.is_none() {
                state.pause_start = Some(pts);
            }
            return;
        }
        if let Some(pause_start) = state.pause_start.take() {
            // SAFETY: pure CMTime arithmetic.
            state.offset = unsafe { CMTime::add(state.offset, CMTime::subtract(pts, pause_start)) };
        }

        let input = match kind {
            SCStreamOutputType::Screen => Some(&state.video),
            SCStreamOutputType::Audio => state.audio.as_ref(),
            SCStreamOutputType::Microphone => state.microphone.as_ref(),
            _ => None,
        };
        let Some(input) = input else {
            return;
        };
        // SAFETY: plain property read.
        if !unsafe { input.isReadyForMoreMediaData() } {
            return;
        }
        let Some(retimed) = retime(buffer, state.offset) else {
            return;
        };
        // SAFETY: a valid sample buffer for this input's media type.
        let appended = unsafe { input.appendSampleBuffer(&retimed) };
        if !appended {
            // SAFETY: plain property reads.
            let message = unsafe { state.writer.error() }
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "the video writer stopped".into());
            tracing::error!(error = %message, ?kind, "could not append sample");
            state.error = Some(message);
            return;
        }
        if kind == SCStreamOutputType::Screen {
            // SAFETY: pure CMTime arithmetic.
            state.last_video = Some(unsafe { CMTime::subtract(pts, state.offset) });
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[ivars = Retained<StreamOutput>]
    struct MicrophoneOutput;

    unsafe impl NSObjectProtocol for MicrophoneOutput {}

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for MicrophoneOutput {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            boost(buffer);
            self.ivars().handle(buffer, SCStreamOutputType::Microphone);
        }
    }
);

/// Gain for the macOS 14 microphone path. ScreenCaptureKit's microphone
/// (macOS 15) applies automatic gain: in tests its level varied by about
/// 12 dB between recordings and peaked up to 17 dB above the raw input.
/// 6x (about 15.6 dB) puts the raw input's peaks at the louder end of
/// that range; `soft_limit` catches anything louder.
const MICROPHONE_GAIN: f32 = 6.0;
/// Above this level, `soft_limit` bends peaks instead of clipping them.
const LIMIT_KNEE: f32 = 0.8;

/// Applies `MICROPHONE_GAIN` to a buffer of 32-bit float samples (the
/// format the capture output is set to), in place.
fn boost(buffer: &CMSampleBuffer) {
    // SAFETY: the capture output delivers float PCM it does not reuse
    // until the delegate returns; the pointer covers `length` bytes when
    // the data is contiguous, which is checked first.
    unsafe {
        let Some(block) = buffer.data_buffer() else {
            return;
        };
        let length = block.data_length();
        if length == 0 || !block.is_range_contiguous(0, length) {
            return;
        }
        let mut data: *mut std::ffi::c_char = ptr::null_mut();
        if block.data_pointer(0, ptr::null_mut(), ptr::null_mut(), &mut data) != 0
            || data.is_null()
            || !(data as usize).is_multiple_of(std::mem::align_of::<f32>())
        {
            return;
        }
        let samples = std::slice::from_raw_parts_mut(data.cast::<f32>(), length / 4);
        for s in samples {
            *s = soft_limit(*s * MICROPHONE_GAIN);
        }
    }
}

/// Leaves samples up to the knee alone and eases louder ones toward full
/// scale, so boosted speech does not clip harshly.
fn soft_limit(x: f32) -> f32 {
    let magnitude = x.abs();
    if magnitude <= LIMIT_KNEE {
        return x;
    }
    let room = 1.0 - LIMIT_KNEE;
    let bent = LIMIT_KNEE + room * ((magnitude - LIMIT_KNEE) / room).tanh();
    bent.copysign(x)
}

/// Microphone capture where ScreenCaptureKit cannot record it (before
/// macOS 15): a capture session delivering to the recording queue, so its
/// samples are written exactly like ScreenCaptureKit's.
struct Microphone {
    session: Retained<AVCaptureSession>,
    _delegate: Retained<MicrophoneOutput>,
}

impl Microphone {
    fn start(output: &Retained<StreamOutput>, queue: &DispatchQueue) -> Result<Microphone, String> {
        // SAFETY: standard capture session setup with the default
        // microphone; the delegate and queue outlive the session's use.
        unsafe {
            let audio = AVMediaTypeAudio.ok_or("no audio media type")?;
            let device = AVCaptureDevice::defaultDeviceWithMediaType(audio)
                .ok_or("no microphone is connected")?;
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| e.localizedDescription().to_string())?;
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            if !session.canAddInput(&input) {
                return Err("the microphone cannot be used".into());
            }
            session.addInput(&input);
            let data = AVCaptureAudioDataOutput::new();
            // Mono 48 kHz float PCM, the format the writer's microphone
            // input expects to encode.
            let number = |n: f64| -> Retained<AnyObject> { NSNumber::new_f64(n).into() };
            let keys = [
                NSString::from_str("AVFormatIDKey"),
                NSString::from_str("AVSampleRateKey"),
                NSString::from_str("AVNumberOfChannelsKey"),
                NSString::from_str("AVLinearPCMBitDepthKey"),
                NSString::from_str("AVLinearPCMIsFloatKey"),
                NSString::from_str("AVLinearPCMIsNonInterleaved"),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            const LPCM: f64 = 0x6c70_636d as f64; // 'lpcm'
            let settings = NSDictionary::from_retained_objects(
                &key_refs,
                &[
                    number(LPCM),
                    number(48_000.0),
                    number(1.0),
                    number(32.0),
                    number(1.0),
                    number(0.0),
                ],
            );
            data.setAudioSettings(Some(&settings));
            let delegate: Retained<MicrophoneOutput> = {
                let this = MicrophoneOutput::alloc().set_ivars(output.clone());
                msg_send![super(this), init]
            };
            data.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(queue),
            );
            if !session.canAddOutput(&data) {
                return Err("the microphone cannot be recorded".into());
            }
            session.addOutput(&data);
            session.commitConfiguration();
            session.startRunning();
            Ok(Microphone {
                session,
                _delegate: delegate,
            })
        }
    }

    fn stop(&self) {
        // SAFETY: stopping a running session.
        unsafe { self.session.stopRunning() };
    }
}

/// A copy of `buffer` with its timestamps moved back by `offset`.
fn retime(buffer: &CMSampleBuffer, offset: CMTime) -> Option<CFRetained<CMSampleBuffer>> {
    // SAFETY: pure CMTime query.
    if unsafe { offset.seconds() } == 0.0 {
        // SAFETY: retaining a valid sample buffer.
        return Some(unsafe { CFRetained::retain(NonNull::from(buffer)) });
    }
    let mut needed: isize = 0;
    // SAFETY: querying the number of timing entries.
    unsafe { buffer.sample_timing_info_array(0, ptr::null_mut(), &mut needed) };
    let mut timing = vec![
        CMSampleTimingInfo {
            duration: invalid_time(),
            presentationTimeStamp: invalid_time(),
            decodeTimeStamp: invalid_time(),
        };
        needed.max(0) as usize
    ];
    // SAFETY: `timing` has room for `needed` entries.
    let status =
        unsafe { buffer.sample_timing_info_array(needed, timing.as_mut_ptr(), ptr::null_mut()) };
    if status != 0 {
        return None;
    }
    for t in &mut timing {
        // SAFETY: pure CMTime arithmetic on valid times.
        unsafe {
            if t.presentationTimeStamp.flags.contains(CMTimeFlags::Valid) {
                t.presentationTimeStamp = CMTime::subtract(t.presentationTimeStamp, offset);
            }
            if t.decodeTimeStamp.flags.contains(CMTimeFlags::Valid) {
                t.decodeTimeStamp = CMTime::subtract(t.decodeTimeStamp, offset);
            }
        }
    }
    let mut out: *mut CMSampleBuffer = ptr::null_mut();
    // SAFETY: valid buffer and timing array; `out` receives a +1 reference.
    let status = unsafe {
        CMSampleBuffer::create_copy_with_new_timing(
            None,
            buffer,
            timing.len() as isize,
            timing.as_ptr(),
            NonNull::from(&mut out),
        )
    };
    // SAFETY: on success `out` is a new, owned sample buffer.
    (status == 0)
        .then(|| NonNull::new(out))
        .flatten()
        .map(|p| unsafe { CFRetained::from_raw(p) })
}

fn invalid_time() -> CMTime {
    CMTime {
        value: 0,
        timescale: 0,
        flags: CMTimeFlags(0),
        epoch: 0,
    }
}

/// A running recording.
pub struct Recorder {
    stream: Retained<SCStream>,
    output: Retained<StreamOutput>,
    microphone: Option<Microphone>,
    queue: DispatchRetained<DispatchQueue>,
    path: PathBuf,
}

impl Recorder {
    /// Starts capturing. Blocks until ScreenCaptureKit has started (or
    /// failed), which takes a fraction of a second.
    pub fn start(options: Options) -> Result<Recorder, String> {
        let state = new_writer(&options)?;
        let output: Retained<StreamOutput> = {
            let this = StreamOutput::alloc().set_ivars(Mutex::new(state));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        };

        // SAFETY: setting plain configuration properties.
        let config = unsafe {
            let config = SCStreamConfiguration::new();
            config.setWidth(options.width);
            config.setHeight(options.height);
            config.setMinimumFrameInterval(CMTime::with_seconds(1.0 / options.fps as f64, 600));
            config.setShowsCursor(options.show_cursor);
            config.setIgnoreShadowsSingleWindow(options.ignore_shadows);
            config.setQueueDepth(6);
            if let Some(r) = options.source_rect {
                config.setSourceRect(CGRect::new(
                    CGPoint::new(r.x, r.y),
                    CGSize::new(r.width, r.height),
                ));
            }
            if macos_at_least(15) {
                config.setShowMouseClicks(options.show_clicks);
                config.setCaptureMicrophone(options.microphone);
            }
            if options.system_audio {
                config.setCapturesAudio(true);
                config.setSampleRate(48_000);
                config.setChannelCount(2);
                config.setExcludesCurrentProcessAudio(true);
            }
            config
        };

        // SAFETY: valid filter and configuration; the delegate outlives the
        // stream (the Recorder keeps both).
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &options.filter,
                &config,
                Some(ProtocolObject::from_ref(&*output)),
            )
        };
        let queue = DispatchQueue::new("com.cleanshotz.recording", None);
        let mut kinds = vec![SCStreamOutputType::Screen];
        if options.system_audio {
            kinds.push(SCStreamOutputType::Audio);
        }
        if options.microphone && macos_at_least(15) {
            kinds.push(SCStreamOutputType::Microphone);
        }
        for kind in kinds {
            // SAFETY: the output implements SCStreamOutput; the queue lives
            // as long as the Recorder.
            unsafe {
                stream.addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    kind,
                    Some(&queue),
                )
            }
            .map_err(|e| e.localizedDescription().to_string())?;
        }

        wait_for(|done| {
            // SAFETY: the completion block has the expected signature.
            unsafe { stream.startCaptureWithCompletionHandler(Some(&done)) }
        })?;
        let microphone = if options.microphone && !macos_at_least(15) {
            match Microphone::start(&output, &queue) {
                Ok(m) => Some(m),
                Err(e) => {
                    // SAFETY: the completion block has the expected signature.
                    let _ = wait_for(|done| unsafe {
                        stream.stopCaptureWithCompletionHandler(Some(&done))
                    });
                    return Err(format!("could not record the microphone: {e}"));
                }
            }
        } else {
            None
        };
        tracing::info!(path = %options.output.display(), width = options.width, height = options.height, "recording started");
        Ok(Recorder {
            stream,
            output,
            microphone,
            queue,
            path: options.output,
        })
    }

    pub fn set_paused(&self, paused: bool) {
        if let Ok(mut state) = self.output.ivars().lock() {
            state.paused = paused;
        }
    }

    /// Stops capturing and finishes the file. `done` runs on the main
    /// thread with the file, or an error.
    pub fn stop(self, done: impl FnOnce(Result<PathBuf, String>) + Send + 'static) {
        let Recorder {
            stream,
            output,
            microphone,
            queue,
            path,
        } = self;
        if let Some(m) = &microphone {
            m.stop();
        }
        let stop_result = wait_for(|done| {
            // SAFETY: the completion block has the expected signature.
            unsafe { stream.stopCaptureWithCompletionHandler(Some(&done)) }
        });
        if let Err(e) = stop_result {
            // Already stopped (for example by the system); finish anyway.
            tracing::debug!(error = %e, "stopping the stream");
        }
        let output = AssertSend(output);
        let keep = AssertSend((stream, path));
        // Finish on the recording queue, after any samples in flight.
        queue.exec_async(move || {
            let output = output.into_inner();
            let (_stream, path) = keep.into_inner();
            let finished = finish(&output);
            let result = finished.map(|()| path);
            DispatchQueue::main().exec_async(move || done(result));
        });
    }
}

/// Marks the inputs finished and waits for the writer to close the file.
fn finish(output: &StreamOutput) -> Result<(), String> {
    let mut state = output
        .ivars()
        .lock()
        .map_err(|_| "recording state poisoned")?;
    state.finishing = true;
    if !state.started {
        // SAFETY: cancelling an unstarted session.
        unsafe { state.writer.cancelWriting() };
        return Err(state
            .error
            .clone()
            .unwrap_or_else(|| "nothing was recorded".into()));
    }
    // SAFETY: inputs and writer are in the writing state.
    unsafe {
        state.video.markAsFinished();
        if let Some(a) = &state.audio {
            a.markAsFinished();
        }
        if let Some(m) = &state.microphone {
            m.markAsFinished();
        }
        if let Some(last) = state.last_video {
            state.writer.endSessionAtSourceTime(last);
        }
    }
    let writer = state.writer.clone();
    drop(state);
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move || {
        let _ = tx.send(());
    });
    // SAFETY: the block has the expected signature.
    unsafe { writer.finishWritingWithCompletionHandler(&block) };
    rx.recv_timeout(Duration::from_secs(30))
        .map_err(|_| "finishing the recording timed out".to_string())?;
    // SAFETY: plain property reads.
    unsafe {
        if writer.status() == AVAssetWriterStatus::Completed {
            Ok(())
        } else {
            Err(writer
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "the recording could not be finished".into()))
        }
    }
}

fn new_writer(options: &Options) -> Result<WriterState, String> {
    let _ = std::fs::remove_file(&options.output);
    let url = NSURL::fileURLWithPath(&NSString::from_str(&options.output.to_string_lossy()));
    // SAFETY: reading AVFoundation constants; valid URL and file type.
    let writer = unsafe {
        AVAssetWriter::assetWriterWithURL_fileType_error(
            &url,
            AVFileTypeMPEG4.expect("AVFoundation constant"),
        )
    }
    .map_err(|e| e.localizedDescription().to_string())?;

    let number = |n: f64| -> Retained<AnyObject> { NSNumber::new_f64(n).into() };
    // SAFETY: reading AVFoundation constants.
    let (
        codec_key,
        width_key,
        height_key,
        compression_key,
        bitrate_key,
        fps_key,
        keyframe_key,
        codec,
    ) = unsafe {
        (
            AVVideoCodecKey.expect("AVFoundation constant"),
            AVVideoWidthKey.expect("AVFoundation constant"),
            AVVideoHeightKey.expect("AVFoundation constant"),
            AVVideoCompressionPropertiesKey.expect("AVFoundation constant"),
            AVVideoAverageBitRateKey.expect("AVFoundation constant"),
            AVVideoExpectedSourceFrameRateKey.expect("AVFoundation constant"),
            AVVideoMaxKeyFrameIntervalKey.expect("AVFoundation constant"),
            match options.codec {
                Codec::Hevc => AVVideoCodecTypeHEVC,
                _ => AVVideoCodecTypeH264,
            }
            .expect("AVFoundation constant"),
        )
    };
    let compression = NSDictionary::from_retained_objects(
        &[bitrate_key, fps_key, keyframe_key],
        &[
            number(options.bitrate as f64),
            number(options.fps as f64),
            number((options.fps * 2) as f64),
        ],
    );
    let video_settings = NSDictionary::from_retained_objects(
        &[codec_key, width_key, height_key, compression_key],
        &[
            NSString::from_str(&codec.to_string()).into(),
            number(options.width as f64),
            number(options.height as f64),
            compression.into(),
        ],
    );
    // SAFETY: valid media type and output settings for H.264/HEVC.
    let video = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            AVMediaTypeVideo.expect("AVFoundation constant"),
            Some(&video_settings),
        )
    };
    // SAFETY: plain property write.
    unsafe { video.setExpectsMediaDataInRealTime(true) };
    add_input(&writer, &video)?;

    let audio_input = |channels: f64| -> Result<Retained<AVAssetWriterInput>, String> {
        // AAC; the key strings are the values of AVFormatIDKey and friends.
        const AAC: f64 = 0x6161_6320 as f64; // 'aac '
        let keys = [
            NSString::from_str("AVFormatIDKey"),
            NSString::from_str("AVSampleRateKey"),
            NSString::from_str("AVNumberOfChannelsKey"),
            NSString::from_str("AVEncoderBitRateKey"),
        ];
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let settings = NSDictionary::from_retained_objects(
            &key_refs,
            &[
                number(AAC),
                number(48_000.0),
                number(channels),
                number(128_000.0 * channels / 2.0),
            ],
        );
        // SAFETY: valid media type and AAC output settings.
        let input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                AVMediaTypeAudio.expect("AVFoundation constant"),
                Some(&settings),
            )
        };
        // SAFETY: plain property write.
        unsafe { input.setExpectsMediaDataInRealTime(true) };
        add_input(&writer, &input)?;
        Ok(input)
    };
    let audio = options.system_audio.then(|| audio_input(2.0)).transpose()?;
    let microphone = options.microphone.then(|| audio_input(1.0)).transpose()?;

    // SAFETY: inputs are added; start the writer.
    if !unsafe { writer.startWriting() } {
        // SAFETY: plain property read.
        return Err(unsafe { writer.error() }
            .map(|e| e.localizedDescription().to_string())
            .unwrap_or_else(|| "the video writer could not start".into()));
    }
    Ok(WriterState {
        writer,
        video,
        audio,
        microphone,
        started: false,
        paused: false,
        pause_start: None,
        offset: zero_time(),
        last_video: None,
        finishing: false,
        error: None,
    })
}

fn zero_time() -> CMTime {
    // SAFETY: constructing a CMTime.
    unsafe { CMTime::with_seconds(0.0, 600) }
}

fn add_input(writer: &AVAssetWriter, input: &AVAssetWriterInput) -> Result<(), String> {
    // SAFETY: plain writer calls before writing starts.
    unsafe {
        if !writer.canAddInput(input) {
            return Err("the video writer rejected an input".into());
        }
        writer.addInput(input);
    }
    Ok(())
}

/// Runs `start` with a completion block and waits for it.
fn wait_for(start: impl FnOnce(RcBlock<dyn Fn(*mut NSError)>)) -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let block: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: nil or a valid error.
        let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    start(block);
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => Ok(()),
        Ok(Some(message)) => Err(message),
        Err(_) => Err("ScreenCaptureKit did not respond in time".into()),
    }
}

pub fn macos_at_least(major: isize) -> bool {
    // Debug builds: CLEANSHOTZ_PRETEND_MACOS=14 takes the code paths for
    // older systems (the macOS 14 microphone and click fallbacks).
    #[cfg(debug_assertions)]
    if let Some(pretend) = std::env::var("CLEANSHOTZ_PRETEND_MACOS")
        .ok()
        .and_then(|v| v.parse::<isize>().ok())
    {
        return pretend >= major;
    }
    let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
    version.majorVersion >= major
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_limit_keeps_quiet_samples_and_never_clips() {
        assert_eq!(soft_limit(0.5), 0.5);
        assert_eq!(soft_limit(-0.8), -0.8);
        for x in [0.9f32, 1.5, 6.0, 100.0] {
            let y = soft_limit(x);
            assert!(y > LIMIT_KNEE && y <= 1.0, "{x} -> {y}");
            assert_eq!(soft_limit(-x), -y);
        }
        // Louder stays louder.
        assert!(soft_limit(1.0) < soft_limit(2.0));
    }
}
