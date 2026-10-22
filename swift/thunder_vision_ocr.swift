//
// thunder_vision_ocr.swift
// Native macOS Apple Vision OCR & Window Perception Engine for thunder-memory
//

import Foundation
import Vision
import AppKit
import CoreGraphics

struct OCRItem: Codable {
    let text: String
    let confidence: Float
    let box: [Float] // [x, y, width, height] normalized in 0.0 .. 1.0
}

struct CaptureResult: Codable {
    let timestamp_ms: Int64
    let app_name: String
    let bundle_id: String
    let window_title: String
    let full_text: String
    let avg_confidence: Float
    let items: [OCRItem]
}

func getFrontmostAppInfo() -> (appName: String, bundleId: String, windowTitle: String) {
    var appName = ""
    var bundleId = ""
    var windowTitle = ""

    if let frontApp = NSWorkspace.shared.frontmostApplication {
        appName = frontApp.localizedName ?? ""
        bundleId = frontApp.bundleIdentifier ?? ""
        let pid = frontApp.processIdentifier

        // Get window title for this frontmost process
        let windowList = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
        for win in windowList {
            if let winPid = win[kCGWindowOwnerPID as String] as? Int32, winPid == pid {
                let layer = win[kCGWindowLayer as String] as? Int ?? 0
                let title = win[kCGWindowName as String] as? String ?? ""
                if layer == 0 && !title.isEmpty {
                    windowTitle = title
                    break
                }
            }
        }
    }
    return (appName, bundleId, windowTitle)
}

func performOCR(imagePath: String) -> CaptureResult {
    let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
    let (appName, bundleId, windowTitle) = getFrontmostAppInfo()

    let imageURL = URL(fileURLWithPath: imagePath)
    guard let image = NSImage(contentsOf: imageURL) else {
        return CaptureResult(
            timestamp_ms: nowMs,
            app_name: appName,
            bundle_id: bundleId,
            window_title: windowTitle,
            full_text: "",
            avg_confidence: 0,
            items: []
        )
    }

    var proposedRect = CGRect(origin: .zero, size: image.size)
    guard let cgImage = image.cgImage(forProposedRect: &proposedRect, context: nil, hints: nil) else {
        return CaptureResult(
            timestamp_ms: nowMs,
            app_name: appName,
            bundle_id: bundleId,
            window_title: windowTitle,
            full_text: "",
            avg_confidence: 0,
            items: []
        )
    }

    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.recognitionLanguages = ["zh-Hans", "zh-Hant", "en-US"]
    request.usesLanguageCorrection = true

    let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
    do {
        try handler.perform([request])
        guard let observations = request.results else {
            return CaptureResult(
                timestamp_ms: nowMs,
                app_name: appName,
                bundle_id: bundleId,
                window_title: windowTitle,
                full_text: "",
                avg_confidence: 0,
                items: []
            )
        }

        var items: [OCRItem] = []
        var lines: [String] = []
        var totalConfidence: Float = 0

        for obs in observations {
            if let candidate = obs.topCandidates(1).first {
                let bbox = obs.boundingBox
                let item = OCRItem(
                    text: candidate.string,
                    confidence: candidate.confidence,
                    box: [
                        Float(bbox.origin.x),
                        Float(bbox.origin.y),
                        Float(bbox.size.width),
                        Float(bbox.size.height)
                    ]
                )
                items.append(item)
                lines.append(candidate.string)
                totalConfidence += candidate.confidence
            }
        }

        let avgConfidence = items.isEmpty ? 0 : (totalConfidence / Float(items.count))
        let fullText = lines.joined(separator: "\n")

        return CaptureResult(
            timestamp_ms: nowMs,
            app_name: appName,
            bundle_id: bundleId,
            window_title: windowTitle,
            full_text: fullText,
            avg_confidence: avgConfidence,
            items: items
        )
    } catch {
        return CaptureResult(
            timestamp_ms: nowMs,
            app_name: appName,
            bundle_id: bundleId,
            window_title: windowTitle,
            full_text: "",
            avg_confidence: 0,
            items: []
        )
    }
}

let args = CommandLine.arguments

if args.count > 1 && args[1] != "--capture" {
    // Direct image path passed
    let imagePath = args[1]
    let result = performOCR(imagePath: imagePath)
    let encoder = JSONEncoder()
    encoder.outputFormatting = .sortedKeys
    if let data = try? encoder.encode(result), let json = String(data: data, encoding: .utf8) {
        print(json)
    }
} else {
    // Capture current screen to temp file, run OCR, then delete temp file
    let tmpFile = NSTemporaryDirectory() + "thunder_screen_\(ProcessInfo.processInfo.processIdentifier)_\(Int64(Date().timeIntervalSince1970 * 1000)).png"
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    task.arguments = ["-x", "-t", "png", tmpFile]
    do {
        try task.run()
        task.waitUntilExit()
    } catch {
        // Fallback if screencapture fails
    }

    let result = performOCR(imagePath: tmpFile)
    try? FileManager.default.removeItem(atPath: tmpFile)

    let encoder = JSONEncoder()
    encoder.outputFormatting = .sortedKeys
    if let data = try? encoder.encode(result), let json = String(data: data, encoding: .utf8) {
        print(json)
    }
}
