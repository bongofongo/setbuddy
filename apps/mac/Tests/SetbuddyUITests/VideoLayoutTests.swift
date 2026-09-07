import XCTest
@testable import SetbuddyAV

/// The Swift half of one grammar. `VideoWindowLayout::parse` in Rust is the
/// specification; these are the same cases, so the two cannot drift apart
/// without one of them failing.
final class VideoLayoutTests: XCTestCase {
    func testTheSettingsFormParsesTheSameWayAsRust() {
        XCTAssertEqual(VideoLayout.parse("40%"), .screenFraction(0.4))
        XCTAssertEqual(VideoLayout.parse(" 25 % "), .screenFraction(0.25), "whitespace is forgiven")
        XCTAssertEqual(VideoLayout.parse("fill"), .fill)
        XCTAssertEqual(VideoLayout.parse("FullScreen"), .fullscreen)
        XCTAssertEqual(VideoLayout.parse("1280"), .custom(width: 1280, position: nil, screen: nil))
        XCTAssertEqual(
            VideoLayout.parse("1280+100+50"),
            .custom(width: 1280, position: .init(x: 100, y: 50), screen: nil)
        )
        XCTAssertEqual(
            VideoLayout.parse("1280+100+50/1"),
            .custom(width: 1280, position: .init(x: 100, y: 50), screen: 1)
        )
        XCTAssertEqual(VideoLayout.parse("1280/2"), .custom(width: 1280, position: nil, screen: 2))
    }

    func testMalformedLayoutsAreRejectedRatherThanGuessedAt() {
        XCTAssertNil(VideoLayout.parse("1280/x"), "a screen is an index")
        XCTAssertNil(VideoLayout.parse("0%"))
        XCTAssertNil(VideoLayout.parse("120%"))
        XCTAssertNil(VideoLayout.parse("0"))
        XCTAssertNil(VideoLayout.parse("1280+100"), "a position needs both axes")
        XCTAssertNil(VideoLayout.parse("1280+1+2+3"))
        XCTAssertNil(VideoLayout.parse("wide"))
    }

    /// The pop-out follows the video's shape, so a width is the whole size.
    func testAFrameKeepsTheVideoAspect() throws {
        try XCTSkipIf(NSScreen.screens.isEmpty, "no screen to place a window on")
        let frame = try XCTUnwrap(VideoLayout.parse("50%")?.frame(aspect: 16.0 / 9.0))
        XCTAssertEqual(frame.width / frame.height, 16.0 / 9.0, accuracy: 0.01)
    }
}
