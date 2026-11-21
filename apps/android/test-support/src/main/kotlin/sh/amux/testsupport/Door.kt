package sh.amux.testsupport

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.floatOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * Launch intent extra keys shared between the Android debug door and the
 * `testnet journey android` host driver.
 */
object DoorArgs {
    /** Integer extra naming the loopback TCP port `DoorServer` binds on `127.0.0.1`. */
    const val EXTRA_DOOR_PORT: String = "amux_door_port"

    /** JSON string extra carrying [DoorStartConfig] for a driven launch. */
    const val EXTRA_START_CONFIG: String = "amux_start_config"

    /** Default loopback port used when a launch enables the door without naming a port. */
    const val DEFAULT_PORT: Int = 19876
}

/**
 * Runtime overrides handed to a debug build on launch via [DoorArgs.EXTRA_START_CONFIG].
 */
@Serializable
data class DoorStartConfig(
    @SerialName("data_dir") val dataDir: String? = null,
    @SerialName("device_name") val deviceName: String? = null,
    @SerialName("discovery_scope") val discoveryScope: String = "",
    @SerialName("lan") val lan: Boolean = true,
    @SerialName("lan_bind") val lanBind: String? = null,
    @SerialName("relay_tcp") val relayTcp: String? = null,
    @SerialName("relay_quic") val relayQuic: String? = null,
    @SerialName("relay_root") val relayRoot: String? = null,
    @SerialName("scripted_cloud") val scriptedCloud: Boolean = false,
    @SerialName("use_system_discovery") val useSystemDiscovery: Boolean = false,
    @SerialName("fresh_start") val freshStart: Boolean = false,
)

/**
 * A discovered LAN peer injected through the debug door (`discovered`).
 * Uses a standard UUID string for [hostId] rather than raw bytes so requests
 * remain readable in transcripts and Rust drivers.
 */
@Serializable
data class DoorFound(
    @SerialName("host_id") val hostId: String,
    @SerialName("name") val name: String,
    @SerialName("addrs") val addrs: List<String>,
    @SerialName("scope") val scope: String = "",
    @SerialName("pairing_open") val pairingOpen: Boolean = false,
)

/** A rectangle in density-independent pixels (dp) in screen coordinates. */
@Serializable
data class VisibleFrame(
    val x: Double,
    val y: Double,
    val width: Double,
    val height: Double,
)

/** One element reported by the live Compose semantics tree. */
@Serializable
data class VisibleElement(
    val identifier: String,
    val label: String? = null,
    val value: String? = null,
    val frame: VisibleFrame,
    val enabled: Boolean = true,
    val selected: Boolean? = null,
)

/** Snapshot of what the running app is currently drawing. */
@Serializable
data class VisibleState(
    val screen: String,
    val fontScale: Float = 1.0f,
    val appearance: String = "light",
    val elements: List<VisibleElement>,
    val reconciled: Boolean,
    val unconfirmed: Int = 0,
)

/**
 * Requests accepted by the debug-only loopback NDJSON door.
 *
 * Every message is one flat JSON object per line tagged by `"kind"`, matching
 * the wire shape of the iOS door.
 */
sealed interface DoorRequest {
    data object Ping : DoorRequest
    data object Ready : DoorRequest
    data object Query : DoorRequest
    data class Tap(val identifier: String, val after: Double? = null) : DoorRequest
    data class Type(val identifier: String, val text: String) : DoorRequest
    data object Back : DoorRequest
    data class PairByCode(val host: String, val pin: String) : DoorRequest
    data class Pair(val qr: String) : DoorRequest
    data class Discovered(val found: List<DoorFound>) : DoorRequest
    data class Open(val screen: String, val fixture: String? = null) : DoorRequest
    data object Settle : DoorRequest
    data class Capture(val path: String? = null) : DoorRequest
    data class Connect(
        val relay: String,
        val token: String,
        val user: String,
        val tier: String = "pro",
    ) : DoorRequest

    data class Cloud(val cloud: CloudScript) : DoorRequest
    data class Appearance(val appearance: String) : DoorRequest
    data class FontScale(val scale: Float) : DoorRequest
    data class AwaitReconciled(val seconds: Double) : DoorRequest
    data class AwaitAgent(val agent: String, val seconds: Double) : DoorRequest
    data object Shutdown : DoorRequest
}

/** Replies emitted by the debug-only loopback NDJSON door. */
sealed interface DoorReply {
    data object Ack : DoorReply
    data class Ready(val pid: Int, val reconciled: Boolean) : DoorReply
    data class State(val state: VisibleState) : DoorReply
    data class Captured(
        val path: String? = null,
        val width: Int,
        val height: Int,
        val scale: Float,
        val base64: String? = null,
    ) : DoorReply

    data class Paired(val host: String) : DoorReply
    data class Error(val message: String) : DoorReply
}

/**
 * Flat-tagged NDJSON codec for [DoorRequest], [DoorReply], and [DoorStartConfig].
 */
object DoorCodec {
    private val json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = true
        explicitNulls = false
    }

    fun decodeStartConfig(raw: String): DoorStartConfig =
        json.decodeFromString(DoorStartConfig.serializer(), raw)

    fun encodeStartConfig(config: DoorStartConfig): String =
        json.encodeToString(DoorStartConfig.serializer(), config)

    fun decodeRequest(line: String): DoorRequest {
        val element = try {
            json.parseToJsonElement(line)
        } catch (e: Exception) {
            throw SerializationException("invalid door JSON: ${e.message}", e)
        }
        val obj = element as? JsonObject
            ?: throw SerializationException("door request must be a JSON object")
        val kind = obj.requireString("kind")
        return when (kind) {
            "ping" -> DoorRequest.Ping
            "ready" -> DoorRequest.Ready
            "query" -> DoorRequest.Query
            "tap" -> DoorRequest.Tap(
                identifier = obj.optionalString("identifier")
                    ?: obj.requireString("id"),
                after = obj.optionalDouble("after"),
            )
            "type" -> DoorRequest.Type(
                identifier = obj.optionalString("identifier")
                    ?: obj.requireString("id"),
                text = obj.requireString("text"),
            )
            "back" -> DoorRequest.Back
            "pairByCode" -> DoorRequest.PairByCode(
                host = obj.requireString("host"),
                pin = obj.optionalString("pin") ?: obj.requireString("code"),
            )
            "pair" -> DoorRequest.Pair(
                qr = obj.optionalString("qr") ?: obj.requireString("link"),
            )
            "discovered" -> {
                val foundElement = obj["found"] as? JsonArray
                    ?: throw SerializationException("discovered requires 'found' array")
                val found = foundElement.map { entry ->
                    json.decodeFromJsonElement<DoorFound>(entry)
                }
                DoorRequest.Discovered(found = found)
            }
            "open" -> DoorRequest.Open(
                screen = obj.optionalString("screen") ?: obj.requireString("route"),
                fixture = obj.optionalString("fixture"),
            )
            "settle" -> DoorRequest.Settle
            "capture" -> DoorRequest.Capture(
                path = obj.optionalString("path"),
            )
            "connect" -> DoorRequest.Connect(
                relay = obj.requireString("relay"),
                token = obj.requireString("token"),
                user = obj.requireString("user"),
                tier = obj.optionalString("tier") ?: "pro",
            )
            "cloud" -> {
                val scriptObj = obj["cloud"] as? JsonObject
                    ?: throw SerializationException("cloud requires 'cloud' object")
                DoorRequest.Cloud(cloud = CloudScript.fromJsonObject(scriptObj))
            }
            "appearance" -> DoorRequest.Appearance(
                appearance = obj.requireString("appearance"),
            )
            "fontScale" -> DoorRequest.FontScale(
                scale = obj.optionalFloat("scale") ?: obj.requireFloat("fontScale"),
            )
            "awaitReconciled" -> DoorRequest.AwaitReconciled(
                seconds = obj.requireDouble("seconds"),
            )
            "awaitAgent" -> DoorRequest.AwaitAgent(
                agent = obj.requireString("agent"),
                seconds = obj.requireDouble("seconds"),
            )
            "shutdown" -> DoorRequest.Shutdown
            else -> throw SerializationException("no door request named $kind")
        }
    }

    fun encodeRequest(request: DoorRequest): String {
        val obj = when (request) {
            DoorRequest.Ping -> buildJsonObject { put("kind", "ping") }
            DoorRequest.Ready -> buildJsonObject { put("kind", "ready") }
            DoorRequest.Query -> buildJsonObject { put("kind", "query") }
            is DoorRequest.Tap -> buildJsonObject {
                put("kind", "tap")
                put("identifier", request.identifier)
                request.after?.let { put("after", it) }
            }
            is DoorRequest.Type -> buildJsonObject {
                put("kind", "type")
                put("identifier", request.identifier)
                put("text", request.text)
            }
            DoorRequest.Back -> buildJsonObject { put("kind", "back") }
            is DoorRequest.PairByCode -> buildJsonObject {
                put("kind", "pairByCode")
                put("host", request.host)
                put("pin", request.pin)
            }
            is DoorRequest.Pair -> buildJsonObject {
                put("kind", "pair")
                put("qr", request.qr)
            }
            is DoorRequest.Discovered -> buildJsonObject {
                put("kind", "discovered")
                put("found", json.encodeToJsonElement(request.found))
            }
            is DoorRequest.Open -> buildJsonObject {
                put("kind", "open")
                put("screen", request.screen)
                request.fixture?.let { put("fixture", it) }
            }
            DoorRequest.Settle -> buildJsonObject { put("kind", "settle") }
            is DoorRequest.Capture -> buildJsonObject {
                put("kind", "capture")
                request.path?.let { put("path", it) }
            }
            is DoorRequest.Connect -> buildJsonObject {
                put("kind", "connect")
                put("relay", request.relay)
                put("token", request.token)
                put("user", request.user)
                put("tier", request.tier)
            }
            is DoorRequest.Cloud -> buildJsonObject {
                put("kind", "cloud")
                put("cloud", encodeCloudScript(request.cloud))
            }
            is DoorRequest.Appearance -> buildJsonObject {
                put("kind", "appearance")
                put("appearance", request.appearance)
            }
            is DoorRequest.FontScale -> buildJsonObject {
                put("kind", "fontScale")
                put("scale", request.scale)
            }
            is DoorRequest.AwaitReconciled -> buildJsonObject {
                put("kind", "awaitReconciled")
                put("seconds", request.seconds)
            }
            is DoorRequest.AwaitAgent -> buildJsonObject {
                put("kind", "awaitAgent")
                put("agent", request.agent)
                put("seconds", request.seconds)
            }
            DoorRequest.Shutdown -> buildJsonObject { put("kind", "shutdown") }
        }
        return json.encodeToString(JsonObject.serializer(), obj)
    }

    fun encodeReply(reply: DoorReply): String {
        val obj = when (reply) {
            DoorReply.Ack -> buildJsonObject { put("kind", "ack") }
            is DoorReply.Ready -> buildJsonObject {
                put("kind", "ready")
                put("pid", reply.pid)
                put("reconciled", reply.reconciled)
            }
            is DoorReply.State -> buildJsonObject {
                put("kind", "state")
                put("state", json.encodeToJsonElement(VisibleState.serializer(), reply.state))
            }
            is DoorReply.Captured -> buildJsonObject {
                put("kind", "captured")
                reply.path?.let { put("path", it) }
                put("width", reply.width)
                put("height", reply.height)
                put("scale", reply.scale)
                reply.base64?.let { put("base64", it) }
            }
            is DoorReply.Paired -> buildJsonObject {
                put("kind", "paired")
                put("host", reply.host)
            }
            is DoorReply.Error -> buildJsonObject {
                put("kind", "error")
                put("message", reply.message)
            }
        }
        return json.encodeToString(JsonObject.serializer(), obj)
    }

    fun decodeReply(line: String): DoorReply {
        val obj = json.parseToJsonElement(line).jsonObject
        return when (val kind = obj.requireString("kind")) {
            "ack" -> DoorReply.Ack
            "ready" -> DoorReply.Ready(
                pid = obj.requireInt("pid"),
                reconciled = obj.optionalBoolean("reconciled") ?: false,
            )
            "state" -> DoorReply.State(
                state = json.decodeFromJsonElement(
                    VisibleState.serializer(),
                    obj["state"] ?: throw SerializationException("state reply missing 'state'"),
                ),
            )
            "captured" -> DoorReply.Captured(
                path = obj.optionalString("path"),
                width = obj.requireInt("width"),
                height = obj.requireInt("height"),
                scale = obj.requireFloat("scale"),
                base64 = obj.optionalString("base64"),
            )
            "paired" -> DoorReply.Paired(
                host = obj.requireString("host"),
            )
            "error" -> DoorReply.Error(
                message = obj.requireString("message"),
            )
            else -> throw SerializationException("no door reply named $kind")
        }
    }

    private fun encodeCloudScript(script: CloudScript): JsonObject = buildJsonObject {
        put("signIn", script.signIn)
        put("account", script.account)
        put("email", script.email)
        script.displayName?.let { put("displayName", it) }
        put("reason", script.reason)
        put("entitlement", script.entitlement)
        put("source", script.source)
        script.token?.let { put("token", it) } ?: put("token", JsonNull)
        put("relayHost", script.relayHost)
        put("relayPort", script.relayPort)
        put("deletion", script.deletion)
        put("recordPurchase", script.recordPurchase)
        put("purchaseReason", script.purchaseReason)
        put("manageUrl", script.manageUrl)
        put("latencyMillis", script.latencyMillis)
    }

    private fun JsonObject.requireString(key: String): String =
        optionalString(key) ?: throw SerializationException("missing string field '$key'")

    private fun JsonObject.optionalString(key: String): String? {
        val el: JsonElement = this[key] ?: return null
        if (el is JsonNull) return null
        val prim = el as? JsonPrimitive ?: return null
        return if (prim.isString) prim.content else prim.contentOrNull
    }

    private fun JsonObject.requireInt(key: String): Int =
        this[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.intOrNull
            ?: throw SerializationException("missing int field '$key'")

    private fun JsonObject.requireDouble(key: String): Double =
        optionalDouble(key) ?: throw SerializationException("missing double field '$key'")

    private fun JsonObject.optionalDouble(key: String): Double? =
        this[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.doubleOrNull

    private fun JsonObject.requireFloat(key: String): Float =
        optionalFloat(key) ?: throw SerializationException("missing float field '$key'")

    private fun JsonObject.optionalFloat(key: String): Float? =
        this[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.floatOrNull

    private fun JsonObject.optionalBoolean(key: String): Boolean? =
        this[key]?.takeUnless { it is JsonNull }?.jsonPrimitive?.booleanOrNull
}
