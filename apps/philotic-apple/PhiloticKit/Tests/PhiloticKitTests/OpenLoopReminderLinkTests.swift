import Foundation
import XCTest
@testable import PhiloticKit

final class OpenLoopReminderLinkTests: XCTestCase {
    func testIdentityIsHotelScopedAndContainsNoNodeTitle() throws {
        let a = try OpenLoopReminderLink(hotelURL: URL(string: "https://hotel.test")!, nodeID: "life:loop")
        XCTAssertEqual(a, try OpenLoopReminderLink(hotelURL: URL(string: "https://HOTEL.test:443/")!, nodeID: "life:loop"))
        XCTAssertNotEqual(a, try OpenLoopReminderLink(hotelURL: URL(string: "https://other.test")!, nodeID: "life:loop"))
        XCTAssertNotEqual(a, try OpenLoopReminderLink(hotelURL: URL(string: "https://hotel.test")!, nodeID: "life:other"))
        XCTAssertFalse(a.marker.contains("life:loop"))
        XCTAssertTrue(a.matches(notes: "My notes\n\(a.marker)\nMore notes"))
        XCTAssertFalse(a.matches(notes: a.marker + " extra"))
    }

    func testRejectsCredentialBearingOrNonCanonicalIdentities() {
        for url in ["https://user:secret@hotel.test", "https://hotel.test?token=secret", "https://hotel.test#secret", "file:///tmp"] {
            XCTAssertThrowsError(try OpenLoopReminderLink(hotelURL: URL(string: url)!, nodeID: "life:loop"))
        }
        XCTAssertThrowsError(try OpenLoopReminderLink(hotelURL: URL(string: "https://hotel.test")!, nodeID: "other:loop"))
    }
}
