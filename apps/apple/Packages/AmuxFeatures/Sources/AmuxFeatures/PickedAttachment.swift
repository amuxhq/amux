import Foundation
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
#endif

/// A photo or file the person picked, as the composer attaches it.
public struct PickedAttachment: Equatable, Sendable {
    public var data: Data
    public var name: String
    public var mime: String
    public var image: Bool

    /// The `index`th photo picked from the library, which offers it as
    /// `types`.
    public static func photo(_ data: Data, types: [UTType], index: Int) -> PickedAttachment {
        #if canImport(UIKit)
        let jpeg = UIImage(data: data)?.jpegData(compressionQuality: 0.85) ?? data
        #else
        let jpeg = data
        #endif
        return PickedAttachment(data: jpeg, name: "photo.jpg", mime: "image/jpeg", image: true)
    }

    /// A file picked from Files.
    public static func file(_ data: Data, name: String) -> PickedAttachment {
        let type = UTType(filenameExtension: (name as NSString).pathExtension)
        return PickedAttachment(
            data: data, name: name,
            mime: type?.preferredMIMEType ?? "application/octet-stream",
            image: type?.conforms(to: .image) ?? false)
    }
}
