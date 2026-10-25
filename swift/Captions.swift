// Speech to text for captions, through Apple's SpeechAnalyzer (macOS 26
// and later). It runs on the Mac; a language's model is downloaded once
// the first time that language is used.
//
// The functions are called from Rust on a background thread and block
// until done. Strings they return are allocated with strdup; Rust frees
// them with free().

import AVFoundation
import Foundation
import Speech

/// Progress while a language model downloads, 0 to 1.
public typealias CaptionsProgress = @convention(c) (UnsafeMutableRawPointer?, Double) -> Void

private struct Word: Encodable {
    let text: String
    let start: Double
    let end: Double
}

private struct Outcome: Encodable {
    var words: [Word] = []
    /// The loudness (RMS, 0 to 1) of every 10 ms of the sound.
    var levels: [Float] = []
    var error: String?
}

private struct Locales: Encodable {
    var supported: [String] = []
    var installed: [String] = []
}

private func json<T: Encodable>(_ value: T) -> UnsafeMutablePointer<CChar>? {
    let data = (try? JSONEncoder().encode(value)) ?? Data("{}".utf8)
    return strdup(String(decoding: data, as: UTF8.self))
}

/// Runs `work` and waits for it (the caller is a plain thread, not part of
/// Swift's concurrency pool).
private func blocking<T: Sendable>(_ work: @escaping @Sendable () async -> T) -> T {
    let semaphore = DispatchSemaphore(value: 0)
    nonisolated(unsafe) var result: T?
    Task.detached {
        result = await work()
        semaphore.signal()
    }
    semaphore.wait()
    return result!
}

/// Whether captions can be made on this Mac.
@_cdecl("cleanshotz_captions_available")
public func captionsAvailable() -> Bool {
    if #available(macOS 26, *) {
        return SpeechTranscriber.isAvailable
    }
    return false
}

/// The languages captions can be made in, and those already on the Mac,
/// as JSON: `{"supported": ["en_US", ...], "installed": [...]}`.
@_cdecl("cleanshotz_captions_locales")
public func captionsLocales() -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26, *) else {
        return json(Locales())
    }
    let locales = blocking {
        Locales(
            supported: await SpeechTranscriber.supportedLocales.map(\.identifier).sorted(),
            installed: await SpeechTranscriber.installedLocales.map(\.identifier).sorted()
        )
    }
    return json(locales)
}

/// Transcribes the audio of `path` (an audio or video file) in `locale`, as
/// JSON: `{"words": [{"text": " Hello", "start": 0.5, "end": 0.9}, ...],
/// "levels": [0.01, ...]}`, or `{"error": "..."}`. Downloads the language's model first if needed,
/// reporting `progress`.
@_cdecl("cleanshotz_captions_transcribe")
public func captionsTranscribe(
    _ path: UnsafePointer<CChar>,
    _ locale: UnsafePointer<CChar>,
    _ progress: CaptionsProgress?,
    _ context: UnsafeMutableRawPointer?
) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26, *) else {
        return json(Outcome(error: "Captions need macOS 26 or later."))
    }
    let url = URL(fileURLWithPath: String(cString: path))
    let identifier = String(cString: locale)
    nonisolated(unsafe) let context = context
    let outcome = blocking {
        do {
            let (words, levels) = try await transcribe(url, identifier) { fraction in
                progress?(context, fraction)
            }
            return Outcome(words: words, levels: levels)
        } catch {
            return Outcome(error: error.localizedDescription)
        }
    }
    return json(outcome)
}

@available(macOS 26, *)
private func transcribe(
    _ url: URL,
    _ identifier: String,
    progress: @escaping @Sendable (Double) -> Void
) async throws -> ([Word], [Float]) {
    guard
        let locale = await SpeechTranscriber.supportedLocale(
            equivalentTo: Locale(identifier: identifier))
    else {
        throw CaptionsError("Captions are not available in this language.")
    }
    let transcriber = SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [],
        attributeOptions: [.audioTimeRange])
    if await AssetInventory.status(forModules: [transcriber]) != .installed,
        let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber])
    {
        let watch = request.progress.observe(\.fractionCompleted) { p, _ in
            progress(p.fractionCompleted)
        }
        defer { watch.invalidate() }
        try await request.downloadAndInstall()
    }
    let (audio, temporary) = try await audioFile(for: url)
    defer {
        if let temporary { try? FileManager.default.removeItem(at: temporary) }
    }
    let levels = try loudness(of: AVAudioFile(forReading: audio.url))
    let analyzer = try await SpeechAnalyzer(
        inputAudioFile: audio, modules: [transcriber], finishAfterFile: true)
    _ = analyzer
    var words: [Word] = []
    for try await result in transcriber.results {
        for run in result.text.runs {
            guard let range = run.audioTimeRange else { continue }
            let text = String(result.text[run.range].characters)
            words.append(Word(text: text, start: range.start.seconds, end: range.end.seconds))
        }
    }
    return (words, levels)
}

/// The RMS loudness of every 10 ms of `file`, all channels together.
private func loudness(of file: AVAudioFile) throws -> [Float] {
    let format = file.processingFormat
    let window = max(1, Int(format.sampleRate / 100))
    let channels = Int(format.channelCount)
    guard
        let buffer = AVAudioPCMBuffer(
            pcmFormat: format, frameCapacity: AVAudioFrameCount(window * 100))
    else { return [] }
    var levels: [Float] = []
    var sum: Float = 0
    var count = 0
    while file.framePosition < file.length {
        try file.read(into: buffer)
        let frames = Int(buffer.frameLength)
        guard frames > 0, let data = buffer.floatChannelData else { break }
        for i in 0..<frames {
            for c in 0..<channels {
                let v = data[c][i]
                sum += v * v
            }
            count += 1
            if count == window {
                levels.append((sum / Float(window * channels)).squareRoot())
                sum = 0
                count = 0
            }
        }
    }
    return levels
}

/// The file to read: `url` itself when it has one audio track, otherwise
/// all its audio tracks mixed into a temporary M4A (AVAudioFile reads only
/// the first track; recordings can keep the microphone and the system
/// audio apart).
@available(macOS 26, *)
private func audioFile(for url: URL) async throws -> (AVAudioFile, URL?) {
    let asset = AVURLAsset(url: url)
    let tracks = try await asset.loadTracks(withMediaType: .audio)
    if tracks.isEmpty {
        throw CaptionsError("The recording has no sound.")
    }
    if tracks.count == 1 {
        return (try AVAudioFile(forReading: url), nil)
    }
    let composition = AVMutableComposition()
    let duration = try await asset.load(.duration)
    for track in tracks {
        let target = composition.addMutableTrack(
            withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid)
        try target?.insertTimeRange(
            CMTimeRange(start: .zero, duration: duration), of: track, at: .zero)
    }
    let output = FileManager.default.temporaryDirectory
        .appendingPathComponent("cleanshotz-captions-\(UUID().uuidString).m4a")
    guard
        let export = AVAssetExportSession(
            asset: composition, presetName: AVAssetExportPresetAppleM4A)
    else {
        throw CaptionsError("The sound could not be read.")
    }
    try await export.export(to: output, as: .m4a)
    return (try AVAudioFile(forReading: output), output)
}

private struct CaptionsError: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}
