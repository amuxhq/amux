import Foundation
import ImageIO
import UniformTypeIdentifiers

/// A photo or file the person picked, as the composer attaches it. An
/// attachment is an image only in a type every agent kind reads (PNG,
/// JPEG, GIF or WebP); the agent judges that again by the bytes, so the
/// phone never labels bytes it could not read as one.
public struct PickedAttachment: Equatable, Sendable {
    public var data: Data
    public var name: String
    public var mime: String
    public var image: Bool

    /// The types every agent kind reads as an image.
    private static let native: [UTType] = [.png, .jpeg, .gif, .webP]

    /// The `index`th photo picked from the library, which offers it as
    /// `types`, most faithful first. A PNG (a screenshot) or GIF keeps its
    /// bytes; any other photo is re-encoded as JPEG, which also drops its
    /// location. A photo that cannot be read goes as a file.
    public static func photo(_ data: Data, types: [UTType], index: Int) -> PickedAttachment {
        let base = "photo-\(index)"
        if let type = types.first, [UTType.png, .gif].contains(where: { type.conforms(to: $0) }) {
            return named(data, base: base, type: type, image: true)
        }
        if let jpeg = decoded(data).flatMap({ encoded($0, as: .jpeg) }) {
            return PickedAttachment(data: jpeg, name: "\(base).jpg", mime: "image/jpeg", image: true)
        }
        return named(data, base: base, type: types.first, image: false)
    }

    /// A file picked from Files. An image in a type no agent reads (HEIC,
    /// TIFF) is converted: PNG when it has transparency, else JPEG. One
    /// that cannot be decoded (SVG) goes as a file, like any other file.
    public static func file(_ data: Data, name: String) -> PickedAttachment {
        let type = UTType(filenameExtension: (name as NSString).pathExtension)
        if let type, native.contains(where: { type.conforms(to: $0) }) {
            return PickedAttachment(data: data, name: name, mime: mime(type), image: true)
        }
        if type?.conforms(to: .image) == true, let image = decoded(data) {
            let base = (name as NSString).deletingPathExtension
            if transparent(image.image), let png = encoded(image, as: .png) {
                return PickedAttachment(data: png, name: "\(base).png", mime: "image/png", image: true)
            }
            if let jpeg = encoded(image, as: .jpeg) {
                return PickedAttachment(data: jpeg, name: "\(base).jpg", mime: "image/jpeg", image: true)
            }
        }
        return PickedAttachment(
            data: data, name: name, mime: type.map(mime) ?? "application/octet-stream", image: false)
    }

    private static func named(_ data: Data, base: String, type: UTType?, image: Bool) -> PickedAttachment {
        let suffix = type?.preferredFilenameExtension.map { ".\($0)" } ?? ""
        return PickedAttachment(
            data: data, name: base + suffix, mime: type.map(mime) ?? "application/octet-stream", image: image)
    }

    private static func mime(_ type: UTType) -> String {
        type.preferredMIMEType ?? "application/octet-stream"
    }

    /// An image as read from its bytes, with the orientation its metadata
    /// gives, which re-encoding keeps; none of its other metadata (such as
    /// where a photo was taken) goes along.
    private struct Decoded {
        let image: CGImage
        let orientation: UInt32?
    }

    private static func decoded(_ data: Data) -> Decoded? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else { return nil }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        return Decoded(image: image, orientation: properties?[kCGImagePropertyOrientation] as? UInt32)
    }

    private static func encoded(_ decoded: Decoded, as type: UTType) -> Data? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil)
        else { return nil }
        var options: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: 0.85]
        if let orientation = decoded.orientation { options[kCGImagePropertyOrientation] = orientation }
        CGImageDestinationAddImage(destination, decoded.image, options as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }

    private static func transparent(_ image: CGImage) -> Bool {
        switch image.alphaInfo {
        case .none, .noneSkipFirst, .noneSkipLast: false
        default: true
        }
    }
}
