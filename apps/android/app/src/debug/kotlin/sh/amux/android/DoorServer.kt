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
import sh.amux.testsupport.DoorCodec
import sh.amux.testsupport.DoorReply
import sh.amux.testsupport.DoorRequest

/**
 * Debug-only loopback NDJSON TCP server accepting one JSON [DoorRequest] per
 * line on `127.0.0.1` and writing one JSON [DoorReply] per line.
 *
 * Host-side journey drivers reach this socket via `adb forward tcp:<host> tcp:<device>`.
 */
internal object DoorServer {
    private const val TAG = "AmuxDoorServer"

    private val lock = Any()
    private var activePort: Int? = null
    private var activeTokenBytes: ByteArray? = null
    private var serverSocket: ServerSocket? = null

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
            val boundPort = socket.localPort
            Log.i(TAG, "DoorServer listening on 127.0.0.1:$boundPort")

            val acceptThread = Thread(
                { acceptLoop(socket, tokenBytes) },
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

    private fun stopLocked() {
        val existing = serverSocket
        serverSocket = null
        activePort = null
        activeTokenBytes = null
        if (existing != null) {
            runCatching { existing.close() }
        }
    }

    private fun acceptLoop(server: ServerSocket, expectedToken: ByteArray) {
        while (!server.isClosed) {
            val client = try {
                server.accept()
            } catch (_: Throwable) {
                return
            }
            val worker = Thread(
                { handleConnection(client, expectedToken) },
                "amux-door-conn-${client.port}",
            ).apply {
                isDaemon = true
            }
            worker.start()
        }
    }

    private fun handleConnection(client: Socket, expectedToken: ByteArray) {
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

                val (providedToken, request) = try {
                    DoorCodec.decodeEnvelope(trimmed)
                } catch (e: Throwable) {
                    writeReply(writer, DoorReply.Error("unreadable request: ${e.message}"))
                    continue
                }

                val authorized = providedToken != null &&
                    MessageDigest.isEqual(
                        providedToken.toByteArray(StandardCharsets.UTF_8),
                        expectedToken,
                    )
                if (!authorized) {
                    writeReply(writer, DoorReply.Error("unauthorized door request"))
                    return
                }

                val reply = try {
                    runBlocking { DoorHost.handle(request) }
                } catch (t: Throwable) {
                    DoorReply.Error("door handler failed: ${t.message ?: t::class.simpleName}")
                }

                if (!writeReply(writer, reply)) {
                    return
                }
                if (request is DoorRequest.Shutdown) {
                    return
                }
            }
        }
    }

    private fun writeReply(writer: BufferedWriter, reply: DoorReply): Boolean = try {
        writer.write(DoorCodec.encodeReply(reply))
        writer.newLine()
        writer.flush()
        true
    } catch (_: Throwable) {
        false
    }
}
