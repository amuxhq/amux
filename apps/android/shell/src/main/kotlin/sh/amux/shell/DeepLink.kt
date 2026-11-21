package sh.amux.shell

import java.net.URI
import sh.amux.core.runtime.PairingInvitation

/**
 * Parsed incoming App Link or custom-scheme deep link.
 */
sealed interface AppLink {
    data class Pairing(val invitation: PairingInvitation) : AppLink
    data class AuthCallback(val url: String) : AppLink
}

/**
 * Parses `https://<host>/p#<invitation>` QR/App Links and `amux://callback`
 * OAuth callbacks, refusing malformed links before they become UI state.
 */
object DeepLink {
    const val INSTALL_URL: String = "https://amux.sh/get"

    fun parse(url: String): AppLink? {
        val trimmed = url.trim()
        if (trimmed.isEmpty()) return null
        PairingInvitation.parse(trimmed)?.let { return AppLink.Pairing(it) }
        val uri = try {
            URI(trimmed)
        } catch (_: Exception) {
            return null
        }
        if (uri.scheme?.equals("amux", ignoreCase = true) == true &&
            uri.host?.equals("callback", ignoreCase = true) == true
        ) {
            return AppLink.AuthCallback(trimmed)
        }
        return null
    }
}
