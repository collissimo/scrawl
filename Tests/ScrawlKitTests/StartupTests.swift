#if os(iOS)
import XCTest
import AVFoundation
@testable import ScrawlKit

final class StartupTests: XCTestCase {
  @MainActor func testPreparedOverlayIsPresentedBeforeHardwareStarts() async throws {
    try requireHost()
    let recorder = SuspendedRecorder()
    let scrawl = makeScrawl(recorder)
    let prepared = expectation(description: "transparent strip laid out")
    let drawn = expectation(description: "prepared strip presented")
    let hardware = expectation(description: "hardware starts after presentation")
    scrawl.startupObserver = { event in
      if event.stage == "prewarmLayout" { prepared.fulfill() }
      if event.stage == "firstDraw" { drawn.fulfill() }
    }
    scrawl.prepareToStart()
    await fulfillment(of: [prepared], timeout: 2)
    recorder.didStart = { hardware.fulfill() }
    scrawl.start()
    XCTAssertEqual(recorder.starts, 0)
    await fulfillment(of: [drawn, hardware], timeout: 2, enforceOrder: true)
    recorder.complete(.failure(RecorderError.notRecording))
  }

  @MainActor func testImmediateStopPreservesFirstMovieFrame() async throws {
    try requireHost()
    let recorder = AppFrameRecorder()
    let started: Result<Date, any Error> = await withCheckedContinuation { continuation in
      recorder.start(microphone: false) { continuation.resume(returning: $0) }
    }
    _ = try started.get()
    let url = URL.temporaryDirectory.appending(path: "scrawl-start-test-\(UUID()).mp4")
    defer { try? FileManager.default.removeItem(at: url) }
    let error: (any Error)? = await withCheckedContinuation { continuation in
      recorder.stop(writingTo: url) { continuation.resume(returning: $0) }
    }
    XCTAssertNil(error)
    let asset = AVURLAsset(url: url)
    let tracks = try await asset.loadTracks(withMediaType: .video)
    XCTAssertEqual(tracks.count, 1)
    let duration = try await asset.load(.duration)
    XCTAssertGreaterThan(duration.seconds, 0)
  }

  @MainActor func testCancelDuringWriterPreparationSuppressesLateCompletion() async throws {
    try requireHost()
    let recorder = AppFrameRecorder()
    let completed = expectation(description: "cancelled start cannot complete")
    completed.isInverted = true
    recorder.start(microphone: false) { _ in completed.fulfill() }
    recorder.cancel()
    await fulfillment(of: [completed], timeout: 0.5)
  }

  @MainActor func testOverlayDrawsBeforeAudioStartCompletesAndEarlyDrawingIsAccepted() async throws {
    try requireHost()
    let recorder = SuspendedRecorder()
    let scrawl = makeScrawl(recorder)
    let drawn = expectation(description: "strip content presented while audio is suspended")
    scrawl.startupObserver = { event in
      if event.stage == "firstDraw" { drawn.fulfill() }
    }
    scrawl.start()
    XCTAssertTrue(scrawl.isStarting)
    XCTAssertFalse(scrawl.isRecording)
    scrawl.toggleDrawing()
    XCTAssertTrue(scrawl.isDrawing)
    await fulfillment(of: [drawn], timeout: 3)
    XCTAssertNotNil(recorder.completion, "audio completion remains withheld until after presentation")
    recorder.complete(.failure(RecorderError.notRecording))
    XCTAssertFalse(scrawl.isStarting)
    XCTAssertNil(scrawl.startedAt)
  }

  @MainActor func testEarlySendIsBufferedAndDuplicateStartDoesNotReplaceRecorder() throws {
    try requireHost()
    let recorder = SuspendedRecorder()
    let scrawl = makeScrawl(recorder)
    scrawl.start()
    scrawl.start()
    XCTAssertEqual(recorder.starts, 1)
    scrawl.stop()
    XCTAssertEqual(recorder.stops, 0)
    recorder.complete(.success(Date()))
    XCTAssertEqual(recorder.stops, 1)
    XCTAssertFalse(scrawl.isStarting)
    XCTAssertFalse(scrawl.isRecording)
  }

  @MainActor func testEarlyDiscardIsBuffered() throws {
    try requireHost()
    let recorder = SuspendedRecorder()
    let scrawl = makeScrawl(recorder)
    scrawl.start()
    scrawl.discard()
    XCTAssertFalse(scrawl.isDiscardPending)
    recorder.complete(.success(Date()))
    XCTAssertEqual(recorder.stops, 1)
    XCTAssertTrue(scrawl.isDiscardPending)
  }

  @MainActor private func makeScrawl(_ recorder: SuspendedRecorder) -> Scrawl {
    Scrawl(config: ScrawlConfig(urls: [URL(string: "http://127.0.0.1:1")!], token: "fixture"),
           makeRecorder: { _, _ in recorder })
  }

  @MainActor private func requireHost() throws {
    // Run these with perf/scrawl-start's hosted simulator target, not an unhosted package runner.
    try XCTSkipIf(Scrawl.activeScene == nil, "Requires the ScrawlStart iOS fixture host")
  }
}

@MainActor private final class SuspendedRecorder: ScrawlRecorder {
  var startupTrace: StartupTrace?
  let method = ScrawlCaptureMethod.appFrames
  let isAvailable = true
  let recordsAudio = false
  var onInterruption: (@MainActor @Sendable () -> Void)?
  var completion: (@MainActor @Sendable (Result<Date, any Error>) -> Void)?
  var starts = 0
  var stops = 0
  var didStart: (() -> Void)?
  func start(microphone: Bool, completion: @escaping @MainActor @Sendable (Result<Date, any Error>) -> Void) {
    starts += 1
    self.completion = completion
    didStart?()
  }
  func complete(_ result: Result<Date, any Error>) {
    let completion = completion
    self.completion = nil
    completion?(result)
  }
  func stop(writingTo movie: URL, completion: @escaping @MainActor @Sendable ((any Error)?) -> Void) {
    stops += 1
    completion(RecorderError.notRecording)
  }
  func cancel() { completion = nil }
}
#endif
