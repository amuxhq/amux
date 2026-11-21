package sh.amux.words

import java.net.URI
import java.util.Locale

/**
 * A link an agent wrote that this phone will open when it is tapped: a web
 * page or an email address, and nothing else.
 *
 * Whatever an agent writes may have been put in its mouth by a page or a
 * file it read, and a tap hands the link to whichever app claims its
 * scheme, this one included: an `amux://` link would hand this app a
 * sign-in callback somebody else wrote, and another app's link performs
 * that app's action. Those schemes are drawn as text and do nothing.
 */
@ConsistentCopyVisibility
data class OpenableLink private constructor(val uri: URI) {
    val url: String
        get() = uri.toString()

    companion object {
        private val ALLOWED_SCHEMES: Set<String> = setOf("https", "http", "mailto")

        fun of(uri: URI): OpenableLink? {
            val scheme = uri.scheme?.lowercase(Locale.ROOT) ?: return null
            return if (scheme in ALLOWED_SCHEMES) OpenableLink(uri) else null
        }

        fun parse(string: String): OpenableLink? {
            if (string.isEmpty()) return null
            val uri = runCatching { URI(string) }.getOrNull() ?: return null
            return of(uri)
        }
    }
}
