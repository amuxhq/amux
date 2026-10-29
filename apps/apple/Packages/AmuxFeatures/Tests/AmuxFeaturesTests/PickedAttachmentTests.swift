import UIKit
import UniformTypeIdentifiers
import XCTest
@testable import AmuxFeatures

final class PickedAttachmentTests: XCTestCase {
    /// A small drawn picture, with or without transparency.
    private func drawn(opaque: Bool) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.opaque = opaque
        format.scale = 1
        return UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4), format: format).image { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 2, height: 2))
        }
    }

    func testAPhotoKeepsItsPngAndGifAndTakesItsOwnName() throws {
        let png = try XCTUnwrap(drawn(opaque: false).pngData())
        XCTAssertEqual(
            PickedAttachment.photo(png, types: [.png], index: 2),
            PickedAttachment(data: png, name: "photo-2.png", mime: "image/png", image: true))
        let gif = Data("GIF89a a frame".utf8)
        XCTAssertEqual(
            PickedAttachment.photo(gif, types: [.gif], index: 3),
            PickedAttachment(data: gif, name: "photo-3.gif", mime: "image/gif", image: true))
    }

    func testAnyOtherPhotoGoesAsJpegAndUnreadableBytesAsAFile() throws {
        let heic = try XCTUnwrap(drawn(opaque: true).jpegData(compressionQuality: 1))
        let picked = PickedAttachment.photo(heic, types: [.heic, .jpeg], index: 1)
        XCTAssertEqual(picked.name, "photo-1.jpg")
        XCTAssertEqual(picked.mime, "image/jpeg")
        XCTAssertTrue(picked.image)
        let raw = Data("not a picture".utf8)
        let unread = PickedAttachment.photo(raw, types: [.heic], index: 4)
        XCTAssertFalse(unread.image, "unreadable bytes are labelled an image")
        XCTAssertNotEqual(unread.mime, "image/jpeg")
        XCTAssertEqual(unread.data, raw)
    }

    func testAFileOfAnImageTypeNoAgentReadsIsConvertedOrSentAsAFile() throws {
        let tiff = try XCTUnwrap(
            drawn(opaque: false).cgImage.flatMap { image -> Data? in
                let data = NSMutableData()
                guard let destination = CGImageDestinationCreateWithData(
                    data, UTType.tiff.identifier as CFString, 1, nil) else { return nil }
                CGImageDestinationAddImage(destination, image, nil)
                return CGImageDestinationFinalize(destination) ? data as Data : nil
            })
        let converted = PickedAttachment.file(tiff, name: "scan.tiff")
        XCTAssertEqual(converted.name, "scan.png", "a transparent TIFF becomes a PNG")
        XCTAssertEqual(converted.mime, "image/png")
        XCTAssertTrue(converted.image)
        let svg = Data("<svg xmlns=\"http://www.w3.org/2000/svg\"/>".utf8)
        let file = PickedAttachment.file(svg, name: "logo.svg")
        XCTAssertFalse(file.image, "an SVG goes as a file")
        XCTAssertEqual(file.name, "logo.svg")
        let png = try XCTUnwrap(drawn(opaque: true).pngData())
        XCTAssertEqual(
            PickedAttachment.file(png, name: "chart.png"),
            PickedAttachment(data: png, name: "chart.png", mime: "image/png", image: true))
    }
}
