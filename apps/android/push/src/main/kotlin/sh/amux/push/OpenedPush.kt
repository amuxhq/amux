package sh.amux.push

import kotlinx.serialization.Serializable

/**
 * What a push says once it is opened: which agent on which host, its name,
 * and what it wants of the person.
 */
@Serializable
data class OpenedPush(
    val host: String,
    val agent: String,
    val name: String,
    val notice: Said,
)
