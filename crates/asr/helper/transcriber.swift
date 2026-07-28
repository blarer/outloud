// aqua-speech-helper: bridge from raw PCM on stdin to Apple SpeechTranscriber.
//
// Why a helper process instead of in-process FFI: SpeechAnalyzer is a Swift
// async API with actor-isolated types; binding it directly from Rust would
// mean hand-rolling Swift concurrency runtime interop. A child process with
// a line-oriented JSON protocol is boring, debuggable with a shell pipe,
// and crash-isolated: if the OS speech stack misbehaves, dictation degrades
// instead of taking the app down.
//
// Protocol:
//   stdin:  raw little-endian f32 mono 16kHz PCM, until EOF.
//   stdout: one JSON object per line:
//     {"type":"ready"}                       model installed, analyzer live
//     {"type":"partial","text":"..."}        volatile hypothesis (replaces previous)
//     {"type":"final","text":"..."}          finalized text for a range
//     {"type":"done"}                        input finished, all results flushed
//     {"type":"error","message":"..."}       fatal problem
//
// Build: swiftc -O transcriber.swift -o aqua-speech-helper  (macOS 26+ SDK)

import Foundation
import Speech
import AVFoundation

func emit(_ obj: [String: String]) {
    guard let data = try? JSONSerialization.data(withJSONObject: obj),
          let line = String(data: data, encoding: .utf8) else { return }
    print(line)
    fflush(stdout)
}

func fail(_ message: String) -> Never {
    emit(["type": "error", "message": message])
    exit(1)
}

/// A thread-safe boolean, because the reader thread sets it and the async
/// task reads it after joining.
final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    func set() { lock.lock(); value = true; lock.unlock() }
    func get() -> Bool { lock.lock(); defer { lock.unlock() }; return value }
}

let semaphore = DispatchSemaphore(value: 0)

Task {
    do {
        let locale = Locale(identifier: ProcessInfo.processInfo.environment["AQUA_ASR_LOCALE"] ?? "en_US")

        guard let transcriberLocale = await SpeechTranscriber.supportedLocale(equivalentTo: locale) else {
            fail("locale \(locale.identifier) not supported by SpeechTranscriber")
        }

        // volatileResults gives us the fast partial hypotheses that the
        // two-stage pipeline paints as ghost text.
        //
        // fastResults is load-bearing, not an optimization. Without it,
        // SpeechTranscriber holds every volatile hypothesis internally and
        // releases all of them in one burst when input finishes: measured
        // with real-time-paced audio, 17 partials arrived within 10ms of
        // each other AFTER the whole utterance had been spoken, so the user
        // watched a frozen overlay and then got the full sentence at once.
        // With fastResults, the same audio produced partials spread across
        // the utterance (first at ~1.3s of a 3.3s clip, then every ~1.3s),
        // which is what "live ghost text" actually requires. Neither stdout
        // buffering nor the stdin reader was the cause; instrumentation
        // showed audio reaching the analyzer in real time while zero
        // results came back until end-of-input.
        let transcriber = SpeechTranscriber(
            locale: transcriberLocale,
            transcriptionOptions: [],
            reportingOptions: [.volatileResults, .fastResults],
            attributeOptions: []
        )

        // The model is managed by the OS. If it is not installed yet,
        // request it; this is the "zero-install" property of this backend:
        // the app never downloads or stores weights itself.
        if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
            try await request.downloadAndInstall()
        }

        let analyzer = SpeechAnalyzer(modules: [transcriber])
        guard let audioFormat = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            fail("no compatible audio format for SpeechTranscriber")
        }

        let (inputSequence, inputBuilder) = AsyncStream.makeStream(of: AnalyzerInput.self)
        try await analyzer.start(inputSequence: inputSequence)

        // Result pump: volatile results replace, finalized results append.
        let resultsTask = Task {
            var finalText = ""
            do {
                for try await result in transcriber.results {
                    let text = String(result.text.characters)
                    if result.isFinal {
                        finalText += text
                        emit(["type": "final", "text": finalText])
                    } else {
                        emit(["type": "partial", "text": finalText + text])
                    }
                }
            } catch {
                emit(["type": "error", "message": "results stream: \(error)"])
            }
        }

        emit(["type": "ready"])

        // Input pump: read raw f32le 16k mono from stdin, convert to the
        // analyzer's preferred format (usually also 16k mono, but let the
        // converter decide rather than assuming).
        guard let sourceFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 16_000,
            channels: 1,
            interleaved: false
        ) else { fail("cannot build source format") }

        guard let converter = AVAudioConverter(from: sourceFormat, to: audioFormat) else {
            fail("cannot build converter to \(audioFormat)")
        }

        // Whether any audio ever reached the analyzer. See the guard before
        // `inputBuilder.finish()` for why this matters.
        let pushedAny = Flag()

        func pushSamples(_ data: Data) {
            let count = data.count / MemoryLayout<Float32>.size
            guard count > 0 else { return }
            guard let inBuf = AVAudioPCMBuffer(pcmFormat: sourceFormat, frameCapacity: AVAudioFrameCount(count)) else { return }
            inBuf.frameLength = AVAudioFrameCount(count)
            data.withUnsafeBytes { raw in
                let src = raw.bindMemory(to: Float32.self)
                inBuf.floatChannelData![0].update(from: src.baseAddress!, count: count)
            }
            let ratio = audioFormat.sampleRate / sourceFormat.sampleRate
            let outCapacity = AVAudioFrameCount(Double(count) * ratio) + 16
            guard let outBuf = AVAudioPCMBuffer(pcmFormat: audioFormat, frameCapacity: outCapacity) else { return }
            var fed = false
            var convError: NSError?
            converter.convert(to: outBuf, error: &convError) { _, status in
                if fed {
                    status.pointee = .noDataNow
                    return nil
                }
                fed = true
                status.pointee = .haveData
                return inBuf
            }
            if let e = convError {
                emit(["type": "error", "message": "convert: \(e)"])
                return
            }
            inputBuilder.yield(AnalyzerInput(buffer: outBuf))
            pushedAny.set()
        }

        // Read stdin on a dedicated OS thread. The naive approach (reading
        // inside this async Task) blocks a cooperative-pool thread, which
        // starves the results Task and turns live partials into 4-second
        // bursts. Measured before/after: bursty -> per-chunk delivery.
        let readerDone = DispatchSemaphore(value: 0)
        let reader = Thread {
            let stdinHandle = FileHandle.standardInput
            let chunkBytes = 3200 * MemoryLayout<Float32>.size // 200ms
            var pending = Data()
            while true {
                let data = stdinHandle.availableData
                if data.isEmpty { break } // EOF
                pending.append(data)
                while pending.count >= chunkBytes {
                    let chunk = pending.prefix(chunkBytes)
                    pending.removeFirst(chunkBytes)
                    pushSamples(Data(chunk))
                }
            }
            if !pending.isEmpty {
                pushSamples(pending)
            }
            readerDone.signal()
        }
        reader.stackSize = 1 << 20
        reader.start()

        // Wait for EOF without blocking the cooperative pool.
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            DispatchQueue.global().async {
                readerDone.wait()
                cont.resume()
            }
        }

        inputBuilder.finish()
        // `finalizeAndFinishThroughEndOfInput()` never returns when the
        // analyzer was started but never received a single buffer: it waits
        // on an end-of-input marker that only exists once audio has flowed.
        // That happens for real whenever the user taps the hotkey without
        // speaking, or releases before the segmenter's onset debounce
        // elapses, and it wedged the whole daemon: the helper hung, aquad's
        // 30s finalize deadline fired, and the overlay was left stuck in
        // Transcribing before reporting a recognizer fault. An utterance
        // with no audio has one honest answer, so give it directly.
        if pushedAny.get() {
            try await analyzer.finalizeAndFinishThroughEndOfInput()
            _ = await resultsTask.result
        } else {
            resultsTask.cancel()
            emit(["type": "final", "text": ""])
        }
        emit(["type": "done"])
        semaphore.signal()
    } catch {
        fail("\(error)")
    }
}

semaphore.wait()
