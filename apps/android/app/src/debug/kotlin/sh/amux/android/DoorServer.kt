package sh.amux.android

import android.util.Log
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import sh.amux.testsupport.DoorCodec
import sh.amux.testsupport.DoorReply

/**
 * Debug-only loopback NDJSON TCP server accepting one JSON door request per
 * line on `127.0.0.1` and writing one JSON reply per line.
 *
 * Host-side journey drivers reach this socket via `adb forward tcp:<host> tcp:<device>`.
 */
internal object DoorServer {
    private const val TAG = "AmuxDoorServer"
    private const val REDACTED = "<redacted>"

    private val json = Json { ignoreUnknownKeys = true }
    private val jsonSecretPattern = Regex(
        """("(?:door_token|doorToken|refresh_token|refreshToken)"\s*:\s*")([^"\\]*(?:\\.[^"\\]*)*)(")""",
    )

    private val lock = Any()
    private var activePort: Int? = null
    private var activeTokenBytes: ByteArray? = null
    private var activeTokenString: String? = null
    private var serverSocket: ServerSocket? = null
    private val activeClients = mutableSetOf<Socket>()

    fun start(port: Int, token: String) {
        require(token.isNotBlank()) { "door auth token must not be blank" }
        val tokenBytes = token.toByteArray(StandardCharsets.UTF_8)
        synchronized(lock) {
            if (activePort == port &&
                serverSocket?.isClosed == false &&
                activeTokenBytes?.let { MessageDigest.isEqual(it, tokenBytes) } == true
            ) {
                return
            }
            stopLocked()
            val loopback = InetAddress.getByName("127.0.0.1")
            val socket = ServerSocket().apply {
                reuseAddress = true
                bind(InetSocketAddress(loopback, port), 16)
            }
            serverSocket = socket
            activePort = socket.localPort
            activeTokenBytes = tokenBytes
            activeTokenString = token
            val boundPort = socket.localPort
            Log.i(TAG, "DoorServer listening on 127.0.0.1:$boundPort")

            val acceptThread = Thread(
                { acceptLoop(socket, tokenBytes, token) },
                "amux-door-accept-$boundPort",
            ).apply {
                isDaemon = true
            }
            acceptThread.start()
        }
    }

    fun stop() {
        synchronized(lock) {
            stopLocked()
        }
    }

    internal fun redactSecrets(text: String?, secrets: Collection<String> = emptyList()): String {
        if (text.isNullOrEmpty()) return ""
        var scrubbed = jsonSecretPattern.replace(text) { match ->
            "${match.groupValues[1]}$REDACTED${match.groupValues[3]}"
        }
        val currentToken = synchronized(lock) { activeTokenString }
        val allSecrets = buildList {
            if (!currentToken.isNullOrBlank()) add(currentToken)
            for (secret in secrets) {
                if (secret.isNotBlank()) add(secret)
            }
        }
        for (secret in allSecrets) {
            scrubbed = scrubbed.replace(secret, REDACTED)
        }
        return scrubbed
    }

    private fun stopLocked() {
        val existing = serverSocket
        serverSocket = null
        activePort = null
        activeTokenBytes = null
        activeTokenString = null
        if (existing != null) {
            runCatching { existing.close() }
        }
        val clients = activeClients.toList()
        activeClients.clear()
        for (client in clients) {
            runCatching { client.close() }
        }
    }

    private fun registerClient(server: ServerSocket, client: Socket): Boolean = synchronized(lock) {
        if (serverSocket !== server || server.isClosed) {
            false
        } else {
            activeClients.add(client)
            true
        }
    }

    private fun unregisterClient(client: Socket) {
        synchronized(lock) {
            activeClients.remove(client)
        }
    }

    private fun acceptLoop(server: ServerSocket, expectedToken: ByteArray, expectedTokenText: String) {
        while (!server.isClosed) {
            val client = try {
                server.accept()
            } catch (_: Throwable) {
                return
            }
            if (!registerClient(server, client)) {
                runCatching { client.close() }
                return
            }
            val worker = Thread(
                { handleConnection(client, expectedToken, expectedTokenText) },
                "amux-door-conn-${client.port}",
            ).apply {
                isDaemon = true
            }
            worker.start()
        }
    }

    private fun handleConnection(
        client: Socket,
        expectedToken: ByteArray,
        expectedTokenText: String,
    ) {
        try {
            client.use { socket ->
                val reader = BufferedReader(InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8))
                val writer = BufferedWriter(OutputStreamWriter(socket.getOutputStream(), StandardCharsets.UTF_8))
                while (true) {
                    val line = try {
                        reader.readLine()
                    } catch (_: Throwable) {
                        return
                    } ?: return
                    val trimmed = line.trim()
                    if (trimmed.isEmpty()) continue

                    val obj = try {
                        json.parseToJsonElement(trimmed) as? JsonObject
                    } catch (e: Throwable) {
                        val safe = redactSecrets(e.message, listOf(expectedTokenText))
                        writeReply(writer, DoorReply.Error("unreadable request: $safe"))
                        continue
                    }
                    if (obj == null) {
                        writeReply(writer, DoorReply.Error("unreadable request: expected JSON object"))
                        continue
                    }

                    val kind = obj.optionalString("kind")
                    val providedToken = obj.optionalString("door_token")
                        ?: obj.optionalString("doorToken")
                        ?: if (kind != "connect" && kind != "addAccount") obj.optionalString("token") else null

                    val secrets = listOfNotNull(expectedTokenText, providedToken)
                    val authorized = providedToken != null &&
                        MessageDigest.isEqual(
                            providedToken.toByteArray(StandardCharsets.UTF_8),
                            expectedToken,
                        )
                    if (!authorized) {
                        writeReply(writer, DoorReply.Error("unauthorized door request"))
                        return
                    }

                    if (kind != null && DoorHost.isExtendedRequest(kind, obj)) {
                        val rawReply = try {
                            runBlocking { DoorHost.handleExtended(kind, obj) }
                        } catch (t: Throwable) {
                            val safe = redactSecrets(t.message ?: t::class.simpleName, secrets)
                            DoorCodec.encodeReply(DoorReply.Error("door handler failed: $safe"))
                        }
                        if (!writeRawLine(writer, redactSecrets(rawReply, secrets))) {
                            return
                        }
                        if (kind == "shutdown") {
                            return
                        }
                        continue
                    }

                    val request = try {
                        DoorCodec.decodeEnvelope(trimmed).second
                    } catch (e: Throwable) {
                        val safe = redactSecrets(e.message, secrets)
                        writeReply(writer, DoorReply.Error("unreadable request: $safe"))
                        continue
                    }

                    val reply = try {
                        runBlocking { DoorHost.handle(request) }
                    } catch (t: Throwable) {
                        val safe = redactSecrets(t.message ?: t::class.simpleName, secrets)
                        DoorReply.Error("door handler failed: $safe")
                    }
                    val safeReply = when (reply) {
                        is DoorReply.Error -> DoorReply.Error(redactSecrets(reply.message, secrets))
                        else -> reply
                    }

                    if (!writeReply(writer, safeReply)) {
                        return
                    }
                    if (kind == "shutdown") {
                        return
                    }
                }
            }
        } finally {
            unregisterClient(client)
        }
    }

    private fun writeReply(writer: BufferedWriter, reply: DoorReply): Boolean =
        writeRawLine(writer, DoorCodec.encodeReply(reply))

    private fun writeRawLine(writer: BufferedWriter, line: String): Boolean = try {
        writer.write(line)
        writer.newLine()
        writer.flush()
        true
    } catch (_: Throwable) {
        false
    }

    private fun JsonObject.optionalString(key: String): String? {
        val el = this[key] ?: return null
        if (el is JsonNull) return null
        val prim = el as? JsonPrimitive ?: return null
        return if (prim.isString) prim.content else prim.contentOrNull
    }
}
