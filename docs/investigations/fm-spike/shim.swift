// Feasibility spike: can a Rust `Transformer` backend call Apple's
// on-device Foundation Models through a small C-ABI Swift shim?
//
// This is the integration path docs/investigations/edit-intent.md recommends
// for the long term, so the recommendation should not rest on the framework
// merely existing. What is checked here:
//
//   1. The framework links and the C-ABI symbols export cleanly.
//   2. Availability is queryable WITHOUT the user having enabled Apple
//      Intelligence, so the daemon can degrade honestly instead of crashing
//      or hanging.
//   3. The transform entry point returns null (not a hang, not a trap) when
//      the model is unavailable, which is the behaviour a fallback to
//      llama.cpp depends on.
//
// Build:
//   swiftc -O -emit-library -static -o liboutloud_fm.a shim.swift

import Foundation
import FoundationModels

/// Availability, as a stable integer a Rust caller can match on.
/// 0 available, 1 not enabled, 2 device ineligible, 3 model downloading.
@_cdecl("outloud_fm_availability")
public func outloud_fm_availability() -> Int32 {
    switch SystemLanguageModel.default.availability {
    case .available:
        return 0
    case .unavailable(let reason):
        switch reason {
        case .appleIntelligenceNotEnabled: return 1
        case .deviceNotEligible: return 2
        case .modelNotReady: return 3
        @unknown default: return 99
        }
    @unknown default:
        return 99
    }
}

/// Transform `text` per `instruction`, or return null when unavailable.
/// Caller owns the result and frees it with `outloud_fm_free`.
///
/// Returning null rather than trapping is the whole point: the Rust side
/// falls back to the llama.cpp backend, and failing that to the honest
/// "freeform needs the language model" message.
@_cdecl("outloud_fm_transform")
public func outloud_fm_transform(
    _ text: UnsafePointer<CChar>,
    _ instruction: UnsafePointer<CChar>
) -> UnsafeMutablePointer<CChar>? {
    let text = String(cString: text)
    let instruction = String(cString: instruction)

    guard case .available = SystemLanguageModel.default.availability else {
        return nil
    }

    // The framework's API is async; the Rust caller is a blocking
    // spawn_blocking task, so the await is bridged with a semaphore.
    let semaphore = DispatchSemaphore(value: 0)
    var result: String?
    Task {
        let session = LanguageModelSession(
            instructions: """
            You are a text transformation engine inside a dictation tool. \
            Apply the instruction to the text and output ONLY the \
            transformed text: no commentary, no code fences, no preamble.
            """
        )
        let prompt =
            "TEXT BEGIN\n\(text)\nTEXT END\n\nInstruction: \(instruction)\n\nTransformed text:"
        result = try? await session.respond(to: prompt).content
        semaphore.signal()
    }
    semaphore.wait()

    guard let result else { return nil }
    return strdup(result)
}

@_cdecl("outloud_fm_free")
public func outloud_fm_free(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}
