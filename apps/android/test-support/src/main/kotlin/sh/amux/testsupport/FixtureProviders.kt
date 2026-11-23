package sh.amux.testsupport

import sh.amux.values.Capabilities
import sh.amux.values.HostProvider
import sh.amux.values.Kind
import sh.amux.values.ProviderLabel

/**
 * The agent programs the fixtures name, as a host would name them, and what
 * an agent of each can do: Claude for either Claude kind, Codex, and for an
 * ACP agent OpenCode. The iOS catalogue's fixtures name the same.
 */
object FixtureProviders {
    /** A fixture agent's program and what an agent of its kind can do. */
    fun of(kind: Kind): Pair<ProviderLabel, Capabilities> = when (kind) {
        Kind.Codex -> ProviderLabel(provider = "codex", name = "Codex") to Capabilities(
            stopsJob = true, switchesBack = false, attachable = true, resumable = true, sendsNow = true,
            images = true, files = true, passesUnknownCommands = false,
        )
        Kind.Acp -> ProviderLabel(provider = "opencode", name = "OpenCode") to Capabilities(
            stopsJob = false, switchesBack = false, attachable = false, resumable = true, sendsNow = false,
            images = true, files = true, passesUnknownCommands = true,
        )
        Kind.ClaudePty -> ProviderLabel(provider = "claude", name = "Claude") to Capabilities(
            stopsJob = false, switchesBack = true, attachable = true, resumable = true, sendsNow = true,
            images = true, files = true, passesUnknownCommands = true,
        )
        Kind.ClaudeSdk, Kind.Unspecified -> ProviderLabel(provider = "claude", name = "Claude") to Capabilities(
            stopsJob = true, switchesBack = false, attachable = false, resumable = true, sendsNow = true,
            images = true, files = true, passesUnknownCommands = true,
        )
    }

    /** What an agent a host offers can do once started there. */
    fun capabilities(acp: Boolean, images: Boolean = true): Capabilities = Capabilities(
        stopsJob = !acp, switchesBack = false, attachable = false, resumable = true, sendsNow = !acp,
        images = images, files = true, passesUnknownCommands = true,
    )

    fun claude(signedIn: Boolean?): HostProvider = HostProvider(
        provider = "claude", name = "Claude", signInHint = "Run claude and sign in with /login.", version = "",
        capabilities = capabilities(acp = false), signedIn = signedIn,
    )

    fun codex(signedIn: Boolean?): HostProvider = HostProvider(
        provider = "codex", name = "Codex", signInHint = "Run codex login.", version = "",
        capabilities = capabilities(acp = false), signedIn = signedIn,
    )

    fun opencode(signedIn: Boolean?): HostProvider = HostProvider(
        provider = "opencode", name = "OpenCode", signInHint = "Run opencode auth login in the terminal",
        version = "1.18.33", capabilities = capabilities(acp = true), signedIn = signedIn,
    )
}
