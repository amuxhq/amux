import Foundation

/// How a machine and a directory are written where there is one line for them.
///
/// One formatter for every such line — the conversation's pill and the rows on
/// the home — so an agent is placed the same way
/// wherever it is listed. The full names are never lost: VoiceOver reads them,
/// and the conversation's place sheet shows them whole.
public enum PlaceNames {
    /// The machine's name without the `.local` a Mac appends to it on its own
    /// network. The suffix is the same on every machine, so it tells machines
    /// apart not at all and costs a quarter of the line.
    public static func host(_ name: String) -> String {
        let suffix = ".local"
        guard name.count > suffix.count, name.lowercased().hasSuffix(suffix) else { return name }
        return String(name.dropLast(suffix.count))
    }

    /// The directory the way a shell prompt writes it: the home directory as
    /// `~`, every parent shortened to its first letter, and the directory
    /// itself whole — `/Users/ada/source/amux` is `~/s/amux`.
    ///
    /// The last component is the one a person recognises a project by, and
    /// the parents are there only to tell two of the same name apart, which a
    /// letter each still does. A hidden parent keeps its dot and first letter.
    public static func directory(_ path: String) -> String {
        var parts = path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        guard !parts.isEmpty else { return path }
        var root = path.hasPrefix("/") ? "/" : ""
        // A home directory is recognised by where the platforms put them. The
        // phone cannot ask the machine what its home is, and these are the
        // places a home is on every machine amux runs on. A Mac's
        // /Users/Shared sits beside the homes but is nobody's.
        if path.hasPrefix("/"), parts.count >= 2, ["Users", "home"].contains(parts[0]),
           !(parts[0] == "Users" && parts[1] == "Shared") {
            parts.removeFirst(2)
            root = "~/"
        } else if path.hasPrefix("/"), parts.first == "root" {
            parts.removeFirst()
            root = "~/"
        } else if parts.first == "~" {
            parts.removeFirst()
            root = "~/"
        }
        guard let last = parts.last else { return "~" }
        let parents = parts.dropLast().map { part -> String in
            if part.hasPrefix("."), part.count > 1 { return String(part.prefix(2)) }
            return String(part.prefix(1))
        }
        return root + (parents + [last]).joined(separator: "/")
    }

    /// "~/s/amux · studio": the directory first, because it is what tells one
    /// agent from another on the same machine, then the machine. Either is left
    /// out where it is not known.
    public static func place(host: String?, directory: String) -> String {
        [directory.isEmpty ? nil : Self.directory(directory), host.map(Self.host)]
            .compactMap { $0 }
            .joined(separator: " · ")
    }
}
