package sh.amux.words

import java.util.Locale

/**
 * How a host and a directory are written where there is one line for them.
 *
 * One formatter for every such line — the conversation's pill and the rows on
 * home — so an agent is placed the same way wherever it is listed. Full names
 * are never lost: TalkBack reads them, and the conversation's place sheet shows
 * them whole.
 */
object PlaceNames {
    private const val LOCAL_SUFFIX = ".local"

    /**
     * The host's name without the `.local` a computer appends to it on its own
     * network.
     */
    fun host(name: String): String {
        if (name.length <= LOCAL_SUFFIX.length) return name
        if (!name.lowercase(Locale.ROOT).endsWith(LOCAL_SUFFIX)) return name
        return name.dropLast(LOCAL_SUFFIX.length)
    }

    /**
     * The directory the way a shell prompt writes it: the home directory as
     * `~`, every parent shortened to its first letter, and the directory
     * itself whole — `/Users/ada/source/amux` is `~/s/amux`.
     *
     * A hidden parent keeps its dot and first letter.
     */
    fun directory(path: String): String {
        val (root, parts) = homed(path) ?: return path
        val last = parts.lastOrNull() ?: return "~"
        val parents = parts.dropLast(1).map { part ->
            if (part.startsWith(".") && part.length > 1) part.take(2) else part.take(1)
        }
        return root + (parents + last).joinToString("/")
    }

    /**
     * The directory with only its home written as `~` —
     * `/Users/ada/source/amux` is `~/source/amux` — for where a path is read
     * whole, as when choosing one.
     */
    fun path(path: String): String {
        val (root, parts) = homed(path) ?: return path
        return if (parts.isEmpty()) "~" else root + parts.joinToString("/")
    }

    /**
     * `"~/s/amux · studio"`: the directory first, because it is what tells one
     * agent from another on the same host, then the host. Either is left out
     * where it is not known.
     */
    fun place(host: String?, directory: String): String =
        listOfNotNull(
            directory.takeIf { it.isNotEmpty() }?.let(::directory),
            host?.let(::host),
        ).joinToString(" · ")

    private fun homed(path: String): Pair<String, List<String>>? {
        val parts = path.split('/').filter { it.isNotEmpty() }.toMutableList()
        if (parts.isEmpty()) return null
        var root = if (path.startsWith("/")) "/" else ""
        if (path.startsWith("/") &&
            parts.size >= 2 &&
            (parts[0] == "Users" || parts[0] == "home") &&
            !(parts[0] == "Users" && parts[1] == "Shared")
        ) {
            parts.removeAt(0)
            parts.removeAt(0)
            root = "~/"
        } else if (path.startsWith("/") && parts.first() == "root") {
            parts.removeAt(0)
            root = "~/"
        } else if (parts.first() == "~") {
            parts.removeAt(0)
            root = "~/"
        }
        return root to parts
    }
}
