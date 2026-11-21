package sh.amux.core.runtime

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import sh.amux.values.AgentNameProblem
import sh.amux.values.Catalogue
import sh.amux.values.CommandMatch
import sh.amux.values.CommandView
import sh.amux.values.FormField
import sh.amux.values.FormProblem
import sh.amux.values.FormValue
import sh.amux.values.FrozenReview
import sh.amux.values.NewAgentChoices
import sh.amux.values.NewAgentPick
import sh.amux.values.OnboardingState
import sh.amux.values.ReviewComment
import sh.amux.values.ReviewDoc
import sh.amux.values.SettingsView
import sh.amux.values.SetupStep

/** Why the embedded Rust library did not do what it was asked, in its own words. */
class RuntimeFailure(val description: String) : Exception(description) {
    override fun equals(other: Any?): Boolean =
        this === other || (other is RuntimeFailure && description == other.description)

    override fun hashCode(): Int = description.hashCode()

    override fun toString(): String = "RuntimeFailure($description)"
}

val Throwable.failureDescription: String
    get() = (this as? RuntimeFailure)?.description ?: message ?: toString()

/** A moment of the app's life the embedded runtime is told of. */
enum class Lifecycle(val wireValue: String) {
    Foreground("Foreground"),
    Woken("Woken"),
    Background("Background"),
    Suspending("Suspending"),
    NetworkChanged("NetworkChanged"),
}

/**
 * Stateless typed helpers over [NativeBridge] plus shared bridge JSON decoding.
 */
object Bridge {
    const val STOPPED_REASON: String = "the runtime has stopped"

    val version: String
        get() = NativeBridge.version()

    val attachmentLimit: Long
        get() = NativeBridge.attachmentLimit()

    fun agentNameProblem(name: String): AgentNameProblem? {
        val raw = NativeBridge.agentNameProblem(name) ?: return null
        return decodeNullable(raw)
    }

    fun newAgentSettings(catalogue: Catalogue, chosen: NewAgentChoices): SettingsView? {
        val raw = NativeBridge.newAgentSettings(
            AmuxJson.json.encodeToString(catalogue),
            AmuxJson.json.encodeToString(chosen),
        ) ?: return null
        return decodeNullable(raw)
    }

    fun newAgentPick(
        catalogue: Catalogue,
        chosen: NewAgentChoices,
        pick: NewAgentPick,
    ): NewAgentChoices? {
        val raw = NativeBridge.newAgentPick(
            AmuxJson.json.encodeToString(catalogue),
            AmuxJson.json.encodeToString(chosen),
            AmuxJson.json.encodeToString(pick),
        ) ?: return null
        return decodeNullable(raw)
    }

    fun onboardingNext(
        state: OnboardingState,
        after: SetupStep?,
        finishing: Boolean,
    ): SetupStep? {
        val raw = NativeBridge.onboardingNext(
            AmuxJson.json.encodeToString(state),
            after?.let { AmuxJson.json.encodeToString(it) },
            finishing,
        ) ?: return null
        return decodeNullable(raw)
    }

    fun formProblems(fields: List<FormField>, values: List<FormValue>): List<FormProblem>? {
        val raw = NativeBridge.formProblems(
            AmuxJson.json.encodeToString(fields),
            AmuxJson.json.encodeToString(values),
        ) ?: return null
        return decodeNullable(raw)
    }

    fun commandMatches(commands: List<CommandView>, draft: String): List<CommandMatch> {
        val raw = NativeBridge.commandMatches(
            AmuxJson.json.encodeToString(commands),
            draft,
        ) ?: return emptyList()
        return decodeNullable<List<CommandMatch>>(raw) ?: emptyList()
    }

    fun pastesAsAttachment(text: String): Boolean = NativeBridge.pastesAsAttachment(text)

    fun reviewDoc(review: FrozenReview, comments: List<ReviewComment>): ReviewDoc? {
        val raw = NativeBridge.reviewDoc(
            AmuxJson.json.encodeToString(review),
            AmuxJson.json.encodeToString(comments),
        ) ?: return null
        return decodeNullable(raw)
    }

    internal inline fun <reified T> decodeNullable(raw: String): T? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty() || trimmed == "null") return null
        return try {
            AmuxJson.json.decodeFromString<T>(trimmed)
        } catch (_: Throwable) {
            null
        }
    }

    internal inline fun <reified T> decodeAnswer(raw: String): Result<T> {
        val obj = try {
            AmuxJson.json.parseToJsonElement(raw) as? JsonObject
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure("unreadable bridge answer: ${t.message}"))
        } ?: return Result.failure(RuntimeFailure("unreadable bridge answer"))

        obj["Err"]?.let { errElement ->
            val reason = (errElement as? JsonPrimitive)?.content ?: errElement.toString()
            return Result.failure(RuntimeFailure(reason))
        }
        if ("Ok" !in obj) {
            return Result.failure(RuntimeFailure("bridge answer carried neither Ok nor Err"))
        }
        val okElement = obj["Ok"]!!
        return try {
            Result.success(AmuxJson.json.decodeFromJsonElement<T>(okElement))
        } catch (t: Throwable) {
            Result.failure(RuntimeFailure("could not decode bridge answer: ${t.message}"))
        }
    }

    internal inline fun <reified T> decodeNullableAnswer(raw: String): Result<T?> {
        val obj = try {
            AmuxJson.json.parseToJsonElement(raw) as? JsonObject
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure("unreadable bridge answer: ${t.message}"))
        } ?: return Result.failure(RuntimeFailure("unreadable bridge answer"))

        obj["Err"]?.let { errElement ->
            val reason = (errElement as? JsonPrimitive)?.content ?: errElement.toString()
            return Result.failure(RuntimeFailure(reason))
        }
        if ("Ok" !in obj) {
            return Result.failure(RuntimeFailure("bridge answer carried neither Ok nor Err"))
        }
        val okElement = obj["Ok"]!!
        if (okElement is JsonNull) {
            return Result.success(null)
        }
        return try {
            Result.success(AmuxJson.json.decodeFromJsonElement<T>(okElement))
        } catch (t: Throwable) {
            Result.failure(RuntimeFailure("could not decode bridge answer: ${t.message}"))
        }
    }

    internal fun decodeUnitAnswer(raw: String): Result<Unit> {
        val obj = try {
            AmuxJson.json.parseToJsonElement(raw) as? JsonObject
        } catch (t: Throwable) {
            return Result.failure(RuntimeFailure("unreadable bridge answer: ${t.message}"))
        } ?: return Result.failure(RuntimeFailure("unreadable bridge answer"))

        obj["Err"]?.let { errElement ->
            val reason = (errElement as? JsonPrimitive)?.content ?: errElement.toString()
            return Result.failure(RuntimeFailure(reason))
        }
        return if ("Ok" in obj) {
            Result.success(Unit)
        } else {
            Result.failure(RuntimeFailure("bridge answer carried neither Ok nor Err"))
        }
    }
}
