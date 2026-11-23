package sh.amux.core.runtime

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.json.Json

/** Shared JSON codec matching Rust `serde_json` encoding across the JNI bridge. */
@OptIn(ExperimentalSerializationApi::class)
object AmuxJson {
    val json: Json = Json {
        ignoreUnknownKeys = true
        encodeDefaults = false
        explicitNulls = false
    }
}
