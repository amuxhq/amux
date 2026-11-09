import AmuxCore
import AmuxDesign
import CoreGraphics
import Foundation
import ImageIO
import SwiftUI

/// How a drawn attachment reaches its bytes and opens. Rows sit several views
/// below the chat that holds these, so they travel in the environment.
public struct AttachmentAccess: Sendable {
    /// The bytes where this phone holds them; asking starts the fetch, and
    /// the row is drawn again when they land.
    public var bytes: @MainActor (BlobRef) -> Data?
    public var failed: @MainActor (BlobRef) -> Bool
    public var open: @MainActor (BlobRef) -> Void

    public init(
        bytes: @escaping @MainActor (BlobRef) -> Data?,
        failed: @escaping @MainActor (BlobRef) -> Bool = { _ in false },
        open: @escaping @MainActor (BlobRef) -> Void = { _ in }
    ) {
        self.bytes = bytes
        self.failed = failed
        self.open = open
    }

    static let none = AttachmentAccess(bytes: { _ in nil })
}

extension EnvironmentValues {
    @Entry public var attachmentAccess: AttachmentAccess = .none
    /// Moves when an attachment in this row lands or fails. That changes
    /// nothing a row's views are given, so without this SwiftUI would see
    /// the same view and never ask it again whether its bytes are here.
    @Entry public var attachmentRevision = 0
}

/// An attachment in a message: an image's picture, or a chip that opens the
/// file. What cannot be opened, a paste or a review, stays a plain chip.
struct MessageAttachment: View {
    @Environment(\.attachmentAccess) private var access
    let view: AttachmentView

    var body: some View {
        switch view {
        case .image(let blob) where blob.size <= Thumbnail.sizeLimit:
            ImageCard(blob: blob)
        case .image(let blob), .file(let blob):
            Button { access.open(blob) } label: { AttachmentChip(view: view) }
                .buttonStyle(.amuxControl)
                .thumbTarget(y: 8)
        default:
            AttachmentChip(view: view)
        }
    }
}

/// An image in the chat: a box of a fixed size that shows what is happening,
/// then the picture in the same place. The reference does not say the image's
/// shape, and a box that changed size when it landed would move the chat
/// under the reader. A tap opens it, or fetches it again after a failure.
struct ImageCard: View {
    static let size = CGSize(width: 240, height: 170)

    @Environment(\.design) private var design
    @Environment(\.attachmentAccess) private var access
    @Environment(\.attachmentRevision) private var revision
    let blob: BlobRef
    var named = true
    @State private var decoded: CGImage?
    @State private var unreadable = false

    var body: some View {
        let _ = revision
        // Copying the bytes across the bridge is the costly part, so a
        // picture already made never asks for them.
        let picture = decoded ?? Thumbnail.made(blob.hash)
        let data = picture == nil ? access.bytes(blob) : nil
        let failed = picture == nil && data == nil && access.failed(blob)
        Button { access.open(blob) } label: {
            VStack(alignment: .leading, spacing: 5) {
                frame(picture: picture, failed: failed)
                if named { caption }
            }
        }
        .buttonStyle(.amuxControl)
        .identified("chat.attachment", label: ChatWords.chip(.image(blob)))
        .task(id: data != nil) {
            guard let data, picture == nil else { return }
            let hash = blob.hash
            let made = await Task.detached { Thumbnail.make(data, hash: hash)?.value }.value
            decoded = made
            unreadable = made == nil
        }
    }

    private func frame(picture: CGImage?, failed: Bool) -> some View {
        RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous)
            .fill(design.sunken.color)
            .overlay {
                if let picture {
                    Image(decorative: picture, scale: 1).resizable().scaledToFit()
                } else {
                    waiting(failed: failed)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: design.metrics.controlRadius, style: .continuous))
            .frame(width: Self.size.width, height: Self.size.height)
    }

    /// What the box says with no picture in it. Its glyph and the name below
    /// are what is known while the bytes come.
    private func waiting(failed: Bool) -> some View {
        VStack(spacing: 6) {
            Image(systemName: failed ? "arrow.clockwise" : "photo")
                .font(.system(size: 22, weight: .regular))
            if failed {
                Text("Can’t load. Tap to try again.")
                    .designFont(.caption, design)
            } else if unreadable {
                Text("No preview")
                    .designFont(.caption, design)
            }
        }
        .foregroundStyle(design.inkFaint.color)
        .padding(.horizontal, 12)
    }

    private var caption: some View {
        let (name, detail) = ChatWords.chipParts(.image(blob))
        return HStack(spacing: 0) {
            Text(verbatim: name).lineLimit(1).truncationMode(.middle)
            if !detail.isEmpty {
                Text(verbatim: " · " + detail).lineLimit(1).fixedSize()
            }
        }
        .designFont(.caption, design)
        .foregroundStyle(design.inkMuted.color)
        .frame(maxWidth: Self.size.width, alignment: .leading)
    }
}

/// Decodes an image without reaching for a platform image type, once for each
/// image however often its row is drawn.
public enum Thumbnail {
    /// The largest image fetched by itself to show its picture: a bigger one
    /// waits to be opened, so a chat cannot spend someone's mobile data
    /// unasked. The terminal keeps this number too, `THUMBNAIL_SIZE_LIMIT`.
    static let sizeLimit: UInt64 = 16 * 1024 * 1024

    final class Made: @unchecked Sendable {
        let value: CGImage
        init(_ value: CGImage) { self.value = value }
    }

    nonisolated(unsafe) private static let cache = NSCache<NSData, Made>()

    private static func key(_ hash: [UInt8]) -> NSData { Data(hash) as NSData }

    static func made(_ hash: [UInt8]) -> CGImage? {
        cache.object(forKey: key(hash))?.value
    }

    static func make(_ data: Data, hash: [UInt8]) -> Made? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceThumbnailMaxPixelSize: 900,
                  kCGImageSourceCreateThumbnailWithTransform: true,
              ] as CFDictionary)
        else { return nil }
        let made = Made(image)
        cache.setObject(made, forKey: key(hash))
        return made
    }

    /// Makes the picture ahead of its first drawing.
    public static func prepare(_ data: Data, hash: [UInt8]) -> Bool {
        make(data, hash: hash) != nil
    }

    /// The picture at once, decoding here if need be: for the composer's
    /// small chip, whose image the person just chose.
    static func image(_ blob: BlobRef, bytes: (BlobRef) -> Data?) -> Image? {
        let made = made(blob.hash) ?? bytes(blob).flatMap { make($0, hash: blob.hash)?.value }
        return made.map { Image(decorative: $0, scale: 1) }
    }
}
