#if os(macOS)
import XCTest
@testable import PhiloticApp

@MainActor
final class HotelDesktopTests: XCTestCase {
    func testHotelOriginStripsPathsQueriesAndFragments() {
        XCTAssertEqual(HotelDesktopSession.origin(" https://hotel.example:7700/path?token=secret#fragment ")?.absoluteString,
                       "https://hotel.example:7700/")
    }
    func testHotelOriginRejectsCredentialsAndNonWebSchemes() {
        for address in ["https://user:secret@hotel.example", "file:///etc/passwd", "javascript:alert(1)", "hotel.example", "https://"] {
            XCTAssertNil(HotelDesktopSession.origin(address), address)
        }
    }
    func testDifferentPortsAndSchemesRemainDifferentHotels() {
        XCTAssertNotEqual(HotelDesktopSession.origin("https://hotel.example"), HotelDesktopSession.origin("http://hotel.example"))
        XCTAssertNotEqual(HotelDesktopSession.origin("http://hotel.example:7700"), HotelDesktopSession.origin("http://hotel.example:7701"))
    }
}
#endif
