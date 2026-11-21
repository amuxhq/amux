package sh.amux.android

import android.app.Activity
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.view.Choreographer
import android.view.MotionEvent
import android.view.PixelCopy
import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import androidx.activity.ComponentActivity
import androidx.compose.ui.node.RootForTest
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.AnnotatedString
import java.io.ByteArrayOutputStream
import java.io.File
import java.lang.ref.WeakReference
import java.net.URI
import java.nio.charset.StandardCharsets
import java.util.Base64
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.put
import sh.amux.core.runtime.AccountId
import sh.amux.core.runtime.AmuxCloudService
import sh.amux.core.runtime.CloudEndpoint
import sh.amux.core.runtime.Entitlement
import sh.amux.core.runtime.EntitlementSource
import sh.amux.core.runtime.Grant
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PairingStore
import sh.amux.core.runtime.RuntimeCoordinator
import sh.amux.core.runtime.SignInIntent
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.id
import sh.amux.design.AppearanceMode
import sh.amux.push.PushPrivateKey
import sh.amux.push.Said
import sh.amux.push.SealedPush
import sh.amux.push.Wants
import sh.amux.shell.PairingModal
import sh.amux.shell.Route
import sh.amux.shell.ShellAction
import sh.amux.shell.Tab
import sh.amux.testsupport.CloudCall
import sh.amux.testsupport.CloudScript
import sh.amux.testsupport.DoorArgs
import sh.amux.testsupport.DoorCodec
import sh.amux.testsupport.DoorFound
import sh.amux.testsupport.DoorReply
import sh.amux.testsupport.DoorRequest
import sh.amux.testsupport.ScriptedCloudService
import sh.amux.testsupport.ScriptedCloudState
import sh.amux.testsupport.ScriptedWebAuth
import sh.amux.testsupport.VisibleElement
import sh.amux.testsupport.VisibleFrame
import sh.amux.testsupport.VisibleState
import sh.amux.values.Found
import sh.amux.words.name

/**
 * Debug-only bridge connecting incoming NDJSON door requests to the live
 * [MainActivity], Compose semantics tree, and embedded [Composition].
 */
internal object DoorHost {
    private val mainHandler = Handler(Looper.getMainLooper())
    private val json = Json { ignoreUnknownKeys = true }

    @Volatile
    private var activityRef: WeakReference<Activity>? = null

    @Volatile
    private var composition: Composition? = null

    @Volatile
    private var scriptedCloud: ScriptedCloudService? = null

    @Volatile
    private var activeStartConfigJson: String? = null

    @Volatile
    private var activeCloudUrl: String? = null

    @Volatile
    private var activeInstallDir: File? = null

    private val _fontScaleOverride = MutableStateFlow<Float?>(null)
    val fontScaleOverride: StateFlow<Float?> = _fontScaleOverride.asStateFlow()

    fun attach(activity: Activity, currentComposition: Composition) {
        activityRef = WeakReference(activity)
        val intent = activity.intent
        val doorToken = doorTokenFromIntent(intent)
        if (doorToken == null) {
            composition = currentComposition
            return
        }
        val configured = configureFromIntent(activity, currentComposition, intent)
        if (configured == null) {
            composition = currentComposition
            return
        }
        composition = configured

        val doorPort = doorPortFromIntent(intent)
        if (doorPort != null) {
            DoorServer.start(doorPort, doorToken)
        }
    }

    fun onNewIntent(activity: Activity, intent: Intent?) {
        activityRef = WeakReference(activity)
        val doorToken = doorTokenFromIntent(intent) ?: return
        val doorPort = doorPortFromIntent(intent)
        if (doorPort != null) {
            DoorServer.start(doorPort, doorToken)
        }
    }

    fun detach(activity: Activity) {
        if (activityRef?.get() === activity) {
            activityRef = null
        }
    }

    private fun doorTokenFromIntent(intent: Intent?): String? =
        intent?.getStringExtra(DoorArgs.EXTRA_DOOR_TOKEN)?.trim()?.takeIf { it.isNotEmpty() }

    private fun doorPortFromIntent(intent: Intent?): Int? {
        if (intent == null) return null
        if (intent.hasExtra(DoorArgs.EXTRA_DOOR_PORT)) {
            val port = intent.getIntExtra(DoorArgs.EXTRA_DOOR_PORT, DoorArgs.DEFAULT_PORT)
            if (port in 1..65535) return port
        }
        val portString = intent.getStringExtra(DoorArgs.EXTRA_DOOR_PORT)
        if (!portString.isNullOrBlank()) {
            portString.toIntOrNull()?.takeIf { it in 1..65535 }?.let { return it }
        }
        if (intent.hasExtra(DoorArgs.EXTRA_START_CONFIG)) {
            return DoorArgs.DEFAULT_PORT
        }
        return null
    }

    private fun configureFromIntent(
        activity: Activity,
        currentComposition: Composition,
        intent: Intent?,
    ): Composition? {
        val rawConfig = intent?.getStringExtra(DoorArgs.EXTRA_START_CONFIG)
        if (rawConfig.isNullOrBlank()) {
            return currentComposition
        }
        if (rawConfig == activeStartConfigJson && composition === currentComposition) {
            return currentComposition
        }
        val config = try {
            DoorCodec.decodeStartConfig(rawConfig)
        } catch (_: Throwable) {
            return null
        }
        val rawObj = runCatching { json.parseToJsonElement(rawConfig) as? JsonObject }.getOrNull()
        val cloudUrl = rawObj?.optionalString("cloud_url")?.trim()?.takeIf { it.isNotEmpty() }
        val notificationsDismissed = rawObj?.optionalBoolean("notifications_dismissed") ?: true

        val app = activity.application as? AmuxApplication ?: return null
        val appContext = app.applicationContext

        val installDir = try {
            resolveDoorDataDir(appContext.filesDir, config.dataDir)
        } catch (_: IllegalArgumentException) {
            return null
        }
        val accountsFile = File(installDir.parentFile, "${installDir.name}-accounts.json")

        // Tear down the default composition completely before replacing it.
        currentComposition.shutdown()
        _fontScaleOverride.value = null

        if (config.freshStart) {
            installDir.deleteRecursively()
            accountsFile.delete()
        }

        activeCloudUrl = cloudUrl
        activeInstallDir = installDir

        val parsedCloud = cloudUrl?.let(::parseHostPort)
        val cloudService = when {
            config.scriptedCloud -> {
                val initialState = if (parsedCloud != null) {
                    ScriptedCloudState(
                        relayHost = parsedCloud.first,
                        relayPort = parsedCloud.second,
                    )
                } else {
                    ScriptedCloudState()
                }
                ScriptedCloudService(initialState).also { scriptedCloud = it }
            }
            cloudUrl != null -> {
                scriptedCloud = null
                AmuxCloudService(endpoint = CloudEndpoint.Production.copy(baseUrl = cloudUrl))
            }
            else -> {
                scriptedCloud = null
                null
            }
        }

        val overrides = CompositionOverrides(
            cloud = cloudService,
            authPresenter = if (config.scriptedCloud) ScriptedWebAuth() else null,
            options = RuntimeCoordinator.Options(
                discoveryScope = config.discoveryScope,
                lan = config.lan,
                lanBind = config.lanBind,
                relayTcp = config.relayTcp,
                relayQuic = config.relayQuic,
                relayRoot = config.relayRoot,
            ),
            useSystemDiscovery = config.useSystemDiscovery,
            deviceName = config.deviceName,
            installationDir = installDir,
            accountsFile = accountsFile,
        )

        val driven = Composition(context = appContext, overrides = overrides)
        if (notificationsDismissed) {
            driven.handleShellAction(ShellAction.DismissNotifications, appContext)
        }
        app.replaceCompositionForTest(driven)
        activeStartConfigJson = rawConfig
        return driven
    }

    internal fun resolveDoorDataDir(filesDir: File, requested: String?): File {
        val baseDir = File(filesDir, "amux-door").canonicalFile
        val candidate = if (requested.isNullOrBlank()) {
            File(baseDir, "installation")
        } else {
            val raw = File(requested)
            if (raw.isAbsolute) raw else File(baseDir, requested)
        }.canonicalFile
        val basePath = baseDir.path
        val candidatePath = candidate.path
        require(candidatePath != basePath && candidatePath.startsWith("$basePath${File.separator}")) {
            "door data_dir must remain inside $basePath"
        }
        return candidate
    }

    fun isExtendedRequest(kind: String, obj: JsonObject): Boolean = when (kind) {
        "clear",
        "paste",
        "scroll",
        "screenshot",
        "triggerReport",
        "background",
        "foreground",
        "addAccount",
        "calls",
        "readRuntimeLog",
        "dismissNotifications",
        -> true
        "tap" -> obj.optionalBoolean("long") == true
        "open" -> obj.optionalString("url") != null ||
            obj.optionalString("uri") != null ||
            obj.optionalString("screen")?.let { it.startsWith("https://") || it.startsWith("amux://") } == true
        "connect" -> obj.optionalString("relay").isNullOrBlank()
        "cloud" -> true
        else -> false
    }

    suspend fun handleExtended(kind: String, obj: JsonObject): String =
        withContext(Dispatchers.Main.immediate) {
            when (kind) {
                "clear" -> {
                    val id = obj.optionalString("identifier") ?: obj.optionalString("id")
                        ?: return@withContext encodeError("clear requires 'identifier'")
                    DoorCodec.encodeReply(clear(id))
                }
                "paste" -> {
                    val id = obj.optionalString("identifier") ?: obj.optionalString("id")
                        ?: return@withContext encodeError("paste requires 'identifier'")
                    val text = obj.optionalString("text")
                        ?: return@withContext encodeError("paste requires 'text'")
                    DoorCodec.encodeReply(paste(id, text))
                }
                "scroll" -> {
                    val id = obj.optionalString("identifier") ?: obj.optionalString("id")
                    val direction = obj.optionalString("direction") ?: "bottom"
                    DoorCodec.encodeReply(scroll(id, direction))
                }
                "tap" -> {
                    val id = obj.optionalString("identifier") ?: obj.optionalString("id")
                        ?: return@withContext encodeError("tap requires 'identifier'")
                    DoorCodec.encodeReply(longPress(id))
                }
                "open" -> {
                    val url = obj.optionalString("url")
                        ?: obj.optionalString("uri")
                        ?: obj.optionalString("screen")
                        ?: return@withContext encodeError("open requires 'url' or 'screen'")
                    DoorCodec.encodeReply(openUrl(url))
                }
                "screenshot", "triggerReport" -> {
                    DoorCodec.encodeReply(triggerReport())
                }
                "background" -> {
                    DoorCodec.encodeReply(setBackground(true))
                }
                "foreground" -> {
                    DoorCodec.encodeReply(setBackground(false))
                }
                "connect" -> {
                    val relay = obj.optionalString("relay")?.takeIf { it.isNotBlank() }
                        ?: activeCloudUrl
                        ?: return@withContext encodeError("connect requires 'relay' or configured cloud_url")
                    val token = obj.optionalString("token")
                        ?: return@withContext encodeError("connect requires 'token'")
                    val user = obj.optionalString("user")
                        ?: return@withContext encodeError("connect requires 'user'")
                    val tier = obj.optionalString("tier") ?: "pro"
                    DoorCodec.encodeReply(connect(relay, token, user, tier))
                }
                "addAccount" -> {
                    val relay = obj.optionalString("relay")?.takeIf { it.isNotBlank() }
                        ?: activeCloudUrl
                        ?: return@withContext encodeError("addAccount requires 'relay' or configured cloud_url")
                    val token = obj.optionalString("token")
                        ?: return@withContext encodeError("addAccount requires 'token'")
                    val user = obj.optionalString("user")
                        ?: return@withContext encodeError("addAccount requires 'user'")
                    val tier = obj.optionalString("tier") ?: "pro"
                    DoorCodec.encodeReply(connect(relay, token, user, tier))
                }
                "cloud" -> {
                    val cloud = scriptedCloud
                        ?: return@withContext encodeError("launch did not enable scripted cloud")
                    val scriptObj = obj["cloud"] as? JsonObject
                        ?: return@withContext encodeError("cloud requires 'cloud' object")
                    var script = CloudScript.fromJsonObject(scriptObj)
                    val parsed = activeCloudUrl?.let(::parseHostPort)
                    if (parsed != null) {
                        if ("relayHost" !in scriptObj) {
                            script = script.copy(relayHost = parsed.first)
                        }
                        if ("relayPort" !in scriptObj) {
                            script = script.copy(relayPort = parsed.second)
                        }
                    }
                    cloud.scripted = script.toState()
                    DoorCodec.encodeReply(DoorReply.Ack)
                }
                "calls" -> {
                    val cloud = scriptedCloud
                    val formatted = cloud?.calls.orEmpty().map(::formatCloudCall)
                    val payload = buildJsonObject {
                        put("kind", "calls")
                        put(
                            "cloud",
                            buildJsonArray {
                                for (call in formatted) {
                                    add(JsonPrimitive(call))
                                }
                            },
                        )
                    }
                    json.encodeToString(JsonObject.serializer(), payload)
                }
                "readRuntimeLog" -> {
                    val dir = activeInstallDir
                        ?: return@withContext encodeError("app has not started")
                    val logFile = File(dir, "runtime.log")
                    val content = if (logFile.isFile) {
                        runCatching { logFile.readText(StandardCharsets.UTF_8) }.getOrDefault("")
                    } else {
                        ""
                    }
                    val payload = buildJsonObject {
                        put("kind", "log")
                        put("text", content)
                    }
                    json.encodeToString(JsonObject.serializer(), payload)
                }
                "dismissNotifications" -> {
                    val comp = composition ?: return@withContext encodeError("app has not started")
                    val activity = activityRef?.get() ?: return@withContext encodeError("no activity on screen")
                    comp.handleShellAction(ShellAction.DismissNotifications, activity)
                    awaitFrame()
                    DoorCodec.encodeReply(DoorReply.Ack)
                }
                else -> encodeError("no door request named $kind")
            }
        }

    suspend fun handle(request: DoorRequest): DoorReply = withContext(Dispatchers.Main.immediate) {
        when (request) {
            DoorRequest.Ping -> DoorReply.Ack
            DoorRequest.Ready -> {
                val reconciled = (composition?.runtime?.stores?.applied?.value ?: 0) > 0
                DoorReply.Ready(pid = Process.myPid(), reconciled = reconciled)
            }
            DoorRequest.Query -> query()
            is DoorRequest.Tap -> tap(request.identifier, request.after)
            is DoorRequest.Type -> type(request.identifier, request.text)
            DoorRequest.Back -> back()
            is DoorRequest.PairByCode -> pairByCode(request.host, request.pin)
            is DoorRequest.Pair -> pairByLink(request.qr)
            is DoorRequest.Discovered -> injectDiscovered(request.found)
            is DoorRequest.Open -> open(request.screen, request.fixture)
            DoorRequest.Settle -> {
                settle()
                DoorReply.Ack
            }
            is DoorRequest.Capture -> capture(request.path)
            is DoorRequest.Connect -> connect(request.relay, request.token, request.user, request.tier)
            is DoorRequest.Cloud -> {
                val cloud = scriptedCloud
                    ?: return@withContext DoorReply.Error("launch did not enable scripted cloud")
                cloud.scripted = request.cloud.toState()
                DoorReply.Ack
            }
            is DoorRequest.Appearance -> setAppearance(request.appearance)
            is DoorRequest.FontScale -> setFontScale(request.scale)
            is DoorRequest.AwaitReconciled -> awaitReconciled(request.seconds)
            is DoorRequest.AwaitAgent -> awaitAgent(request.agent, request.seconds)
            DoorRequest.HpkeSelfCheck -> hpkeSelfCheck()
            DoorRequest.Shutdown -> {
                activityRef?.get()?.finishAffinity()
                DoorReply.Ack
            }
        }
    }

    // --- Semantics inspection & interaction --------------------------------

    private data class LocatedNode(
        val rootView: View,
        val node: SemanticsNode,
    )

    private fun composeRoots(): List<Pair<View, RootForTest>> {
        val activity = activityRef?.get()
        val windowViews = runCatching { WindowInspector.getGlobalWindowViews() }
            .getOrElse { listOfNotNull(activity?.window?.decorView) }
            .filter { it.visibility == View.VISIBLE && it.isAttachedToWindow }

        val roots = mutableListOf<Pair<View, RootForTest>>()
        for (windowView in windowViews) {
            collectComposeRoots(windowView, roots)
        }
        return roots
    }

    private fun collectComposeRoots(view: View, out: MutableList<Pair<View, RootForTest>>) {
        if (view is RootForTest) {
            out.add(view to view)
        }
        if (view is ViewGroup) {
            for (i in 0 until view.childCount) {
                collectComposeRoots(view.getChildAt(i), out)
            }
        }
    }

    private fun query(): DoorReply {
        val activity = activityRef?.get()
            ?: return DoorReply.Error("no activity on screen")
        val density = activity.resources.displayMetrics.density.coerceAtLeast(1f)
        val roots = composeRoots()
        if (roots.isEmpty()) {
            return DoorReply.Error("no Compose root attached")
        }

        val elements = mutableListOf<VisibleElement>()
        val loc = IntArray(2)
        for ((rootView, rootForTest) in roots) {
            rootView.getLocationOnScreen(loc)
            val offsetX = loc[0].toDouble()
            val offsetY = loc[1].toDouble()
            val rootNode = rootForTest.semanticsOwner.rootSemanticsNode
            collectVisibleElements(
                node = rootNode,
                offsetX = offsetX,
                offsetY = offsetY,
                density = density.toDouble(),
                out = elements,
            )
        }

        val comp = composition
        val activeStores = comp?.runtime?.stores
        val applied = activeStores?.applied?.value ?: 0
        val screenName = currentScreenName(comp)
        val effectiveFontScale = _fontScaleOverride.value
            ?: activity.resources.configuration.fontScale
        val appearanceName = when (comp?.appearanceMode?.value) {
            AppearanceMode.Dark -> "dark"
            AppearanceMode.Light -> "light"
            AppearanceMode.System, null -> "system"
        }

        return DoorReply.State(
            VisibleState(
                screen = screenName,
                fontScale = effectiveFontScale,
                appearance = appearanceName,
                elements = elements,
                reconciled = applied > 0,
                unconfirmed = 0,
            ),
        )
    }

    private fun collectVisibleElements(
        node: SemanticsNode,
        offsetX: Double,
        offsetY: Double,
        density: Double,
        out: MutableList<VisibleElement>,
    ) {
        val tag = node.config.getOrNull(SemanticsProperties.TestTag)
        if (!tag.isNullOrEmpty()) {
            val bounds = node.boundsInRoot
            val frame = VisibleFrame(
                x = (offsetX + bounds.left) / density,
                y = (offsetY + bounds.top) / density,
                width = bounds.width / density,
                height = bounds.height / density,
            )
            val label = extractLabel(node)
            val value = extractValue(node)
            val enabled = SemanticsProperties.Disabled !in node.config
            val selected = node.config.getOrNull(SemanticsProperties.Selected)
            out.add(
                VisibleElement(
                    identifier = tag,
                    label = label,
                    value = value,
                    frame = frame,
                    enabled = enabled,
                    selected = selected,
                ),
            )
        }
        for (child in node.children) {
            collectVisibleElements(child, offsetX, offsetY, density, out)
        }
    }

    private fun extractLabel(node: SemanticsNode): String? {
        val direct = directLabelParts(node)
        if (direct.isNotEmpty()) {
            return direct.joinToString(", ")
        }
        val nested = mutableListOf<String>()
        collectDescendantText(node, nested, maxDepth = 6)
        return nested.distinct().takeIf { it.isNotEmpty() }?.joinToString(", ")
    }

    private fun directLabelParts(node: SemanticsNode): List<String> {
        val parts = mutableListOf<String>()
        node.config.getOrNull(SemanticsProperties.ContentDescription)
            ?.filter { it.isNotBlank() }
            ?.let { parts.addAll(it) }
        node.config.getOrNull(SemanticsProperties.Text)
            ?.map { it.text }
            ?.filter { it.isNotBlank() }
            ?.let { parts.addAll(it) }
        return parts.distinct()
    }

    private fun collectDescendantText(
        node: SemanticsNode,
        out: MutableList<String>,
        maxDepth: Int,
    ) {
        if (maxDepth <= 0) return
        for (child in node.children) {
            val childParts = directLabelParts(child)
            out.addAll(childParts)
            if (child.config.getOrNull(SemanticsProperties.TestTag).isNullOrEmpty()) {
                collectDescendantText(child, out, maxDepth - 1)
            }
        }
    }

    private fun extractValue(node: SemanticsNode): String? {
        val editable = node.config.getOrNull(SemanticsProperties.EditableText)?.text
        if (editable != null) return editable
        return node.config.getOrNull(SemanticsProperties.StateDescription)
    }

    private fun findByTag(identifier: String): LocatedNode? {
        // Search topmost window first so sheets and dialogs outrank underlying tabs.
        val roots = composeRoots().asReversed()
        for ((rootView, rootForTest) in roots) {
            val found = findNodeInSubtree(rootForTest.semanticsOwner.rootSemanticsNode) {
                it.config.getOrNull(SemanticsProperties.TestTag) == identifier
            }
            if (found != null) {
                return LocatedNode(rootView = rootView, node = found)
            }
        }
        return null
    }

    private fun findNodeInSubtree(
        node: SemanticsNode,
        predicate: (SemanticsNode) -> Boolean,
    ): SemanticsNode? {
        if (predicate(node)) return node
        for (child in node.children) {
            val found = findNodeInSubtree(child, predicate)
            if (found != null) return found
        }
        return null
    }

    private suspend fun tap(identifier: String, after: Double?): DoorReply {
        settle()
        val located = findByTag(identifier)
            ?: return DoorReply.Error("no element named $identifier")
        if (SemanticsProperties.Disabled in located.node.config) {
            return DoorReply.Error("$identifier is disabled")
        }
        if (after != null && after > 0.0) {
            delay((after * 1000.0).toLong())
        }

        // Prefer real Compose Semantics OnClick on the node, a clickable descendant, or ancestor.
        val clickNode = findClickableNode(located.node)
        val clickAction = clickNode?.config?.getOrNull(SemanticsActions.OnClick)?.action
        if (clickAction != null) {
            val invoked = clickAction.invoke()
            if (invoked) {
                awaitFrame()
                return DoorReply.Ack
            }
        }

        // Fall back to dispatching a real MotionEvent tap at the node's center.
        val bounds = located.node.boundsInRoot
        if (bounds.width <= 0f || bounds.height <= 0f) {
            return DoorReply.Error("$identifier has zero bounds and no OnClick action")
        }
        val cx = bounds.center.x
        val cy = bounds.center.y
        val downTime = SystemClock.uptimeMillis()
        val down = MotionEvent.obtain(downTime, downTime, MotionEvent.ACTION_DOWN, cx, cy, 0)
        val up = MotionEvent.obtain(downTime, downTime + 40L, MotionEvent.ACTION_UP, cx, cy, 0)
        try {
            located.rootView.dispatchTouchEvent(down)
            located.rootView.dispatchTouchEvent(up)
        } finally {
            down.recycle()
            up.recycle()
        }
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun longPress(identifier: String): DoorReply {
        settle()
        val located = findByTag(identifier)
            ?: return DoorReply.Error("no element named $identifier")
        if (SemanticsProperties.Disabled in located.node.config) {
            return DoorReply.Error("$identifier is disabled")
        }
        val longClickAction = findLongClickableNode(located.node)
            ?.config
            ?.getOrNull(SemanticsActions.OnLongClick)
            ?.action
        if (longClickAction != null && longClickAction.invoke()) {
            awaitFrame()
            return DoorReply.Ack
        }
        return DoorReply.Error("$identifier has no OnLongClick action")
    }

    private fun findClickableNode(node: SemanticsNode): SemanticsNode? {
        if (node.config.getOrNull(SemanticsActions.OnClick) != null) return node
        val queue = ArrayDeque<SemanticsNode>()
        queue.addAll(node.children)
        while (queue.isNotEmpty()) {
            val current = queue.removeFirst()
            if (current.config.getOrNull(SemanticsActions.OnClick) != null) {
                return current
            }
            queue.addAll(current.children)
        }
        var parent = node.parent
        while (parent != null) {
            if (parent.config.getOrNull(SemanticsActions.OnClick) != null) {
                return parent
            }
            parent = parent.parent
        }
        return null
    }

    private fun findLongClickableNode(node: SemanticsNode): SemanticsNode? {
        if (node.config.getOrNull(SemanticsActions.OnLongClick) != null) return node
        var parent = node.parent
        while (parent != null) {
            if (parent.config.getOrNull(SemanticsActions.OnLongClick) != null) {
                return parent
            }
            parent = parent.parent
        }
        return null
    }

    private suspend fun type(identifier: String, text: String): DoorReply {
        settle()
        val located = findByTag(identifier)
            ?: return DoorReply.Error("no element named $identifier")
        val target = findEditableNode(located.node)
            ?: return DoorReply.Error("$identifier does not take text")
        if (SemanticsProperties.Disabled in target.config) {
            return DoorReply.Error("$identifier is disabled")
        }

        target.config.getOrNull(SemanticsActions.RequestFocus)?.action?.invoke()
        awaitFrame()

        val insertAction = target.config.getOrNull(SemanticsActions.InsertTextAtCursor)?.action
        if (insertAction != null && insertAction.invoke(AnnotatedString(text))) {
            awaitFrame()
            return DoorReply.Ack
        }

        val setTextAction = target.config.getOrNull(SemanticsActions.SetText)?.action
        if (setTextAction != null) {
            val existing = target.config.getOrNull(SemanticsProperties.EditableText)?.text.orEmpty()
            val updated = existing + text
            if (setTextAction.invoke(AnnotatedString(updated))) {
                awaitFrame()
                return DoorReply.Ack
            }
        }
        return DoorReply.Error("failed to type into $identifier")
    }

    private suspend fun paste(identifier: String, text: String): DoorReply {
        settle()
        val located = findByTag(identifier)
            ?: return DoorReply.Error("no element named $identifier")
        val target = findEditableNode(located.node)
            ?: return DoorReply.Error("$identifier does not take text")
        if (SemanticsProperties.Disabled in target.config) {
            return DoorReply.Error("$identifier is disabled")
        }

        target.config.getOrNull(SemanticsActions.RequestFocus)?.action?.invoke()
        awaitFrame()

        val insertAction = target.config.getOrNull(SemanticsActions.InsertTextAtCursor)?.action
        if (insertAction != null && insertAction.invoke(AnnotatedString(text))) {
            awaitFrame()
            return DoorReply.Ack
        }

        val setTextAction = target.config.getOrNull(SemanticsActions.SetText)?.action
        if (setTextAction != null) {
            val existing = target.config.getOrNull(SemanticsProperties.EditableText)?.text.orEmpty()
            if (setTextAction.invoke(AnnotatedString(existing + text))) {
                awaitFrame()
                return DoorReply.Ack
            }
        }
        return DoorReply.Error("failed to paste into $identifier")
    }

    private suspend fun clear(identifier: String): DoorReply {
        settle()
        val located = findByTag(identifier)
            ?: return DoorReply.Error("no element named $identifier")
        val target = findEditableNode(located.node)
            ?: return DoorReply.Error("$identifier does not take text")
        if (SemanticsProperties.Disabled in target.config) {
            return DoorReply.Error("$identifier is disabled")
        }

        target.config.getOrNull(SemanticsActions.RequestFocus)?.action?.invoke()
        awaitFrame()

        val setTextAction = target.config.getOrNull(SemanticsActions.SetText)?.action
        if (setTextAction != null && setTextAction.invoke(AnnotatedString(""))) {
            awaitFrame()
            return DoorReply.Ack
        }
        return DoorReply.Error("failed to clear $identifier")
    }

    private suspend fun scroll(identifier: String?, direction: String): DoorReply {
        settle()
        val located = if (!identifier.isNullOrBlank()) {
            findByTag(identifier)
                ?: return DoorReply.Error("no element named $identifier")
        } else {
            val topRoot = composeRoots().lastOrNull()
                ?: return DoorReply.Error("no Compose root attached")
            val scrollable = findNodeInSubtree(topRoot.second.semanticsOwner.rootSemanticsNode) {
                it.config.getOrNull(SemanticsActions.ScrollBy) != null
            } ?: return DoorReply.Error("no scrollable element on screen")
            LocatedNode(topRoot.first, scrollable)
        }

        val scrollNode = findScrollableNode(located.node)
            ?: return DoorReply.Error("no scrollable ancestor or descendant for ${identifier ?: "screen"}")
        val scrollAction = scrollNode.config.getOrNull(SemanticsActions.ScrollBy)?.action
            ?: return DoorReply.Error("element does not support ScrollBy")

        val deltaY = when (direction.lowercase()) {
            "bottom" -> 10_000f
            "top" -> -10_000f
            "down" -> 600f
            "up" -> -600f
            else -> return DoorReply.Error("unknown scroll direction: $direction")
        }
        val repeats = if (direction.equals("bottom", true) || direction.equals("top", true)) 3 else 1
        repeat(repeats) {
            scrollAction.invoke(0f, deltaY)
            awaitFrame()
        }
        settle()
        return DoorReply.Ack
    }

    private fun findScrollableNode(node: SemanticsNode): SemanticsNode? {
        if (node.config.getOrNull(SemanticsActions.ScrollBy) != null) return node
        var parent = node.parent
        while (parent != null) {
            if (parent.config.getOrNull(SemanticsActions.ScrollBy) != null) return parent
            parent = parent.parent
        }
        val queue = ArrayDeque<SemanticsNode>()
        queue.addAll(node.children)
        while (queue.isNotEmpty()) {
            val current = queue.removeFirst()
            if (current.config.getOrNull(SemanticsActions.ScrollBy) != null) return current
            queue.addAll(current.children)
        }
        return null
    }

    private fun findEditableNode(node: SemanticsNode): SemanticsNode? {
        if (node.config.getOrNull(SemanticsActions.SetText) != null ||
            node.config.getOrNull(SemanticsActions.InsertTextAtCursor) != null
        ) {
            return node
        }
        for (child in node.children) {
            val found = findEditableNode(child)
            if (found != null) return found
        }
        return null
    }

    private suspend fun back(): DoorReply {
        settle()
        val activity = activityRef?.get() as? ComponentActivity
            ?: return DoorReply.Error("no activity on screen")
        activity.onBackPressedDispatcher.onBackPressed()
        awaitFrame()
        return DoorReply.Ack
    }

    // --- Runtime, pairing, lifecycle & navigation --------------------------

    private suspend fun injectDiscovered(entries: List<DoorFound>): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val runtime = comp.runtime.awaitStarted()
            ?: return DoorReply.Error("runtime did not start")
        val converted = mutableListOf<Found>()
        for (entry in entries) {
            val id = HostId.parse(entry.hostId)
                ?: return DoorReply.Error("invalid host_id UUID: ${entry.hostId}")
            converted.add(
                Found(
                    hostId = id.bytes,
                    name = entry.name,
                    addrs = entry.addrs,
                    scope = entry.scope,
                    pairingOpen = entry.pairingOpen,
                ),
            )
        }
        runtime.discovered(converted)
        comp.runtime.discovered(converted)
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun pairByCode(host: String, pin: String): DoorReply {
        val stores = awaitActiveStores()
            ?: return DoorReply.Error("nothing is running")
        val hostId = HostId.parse(host)
        val known = if (hostId != null) {
            stores.hosts.snapshot.known(hostId)
        } else {
            (stores.hosts.snapshot.hosts + stores.hosts.snapshot.discovered)
                .firstOrNull { it.name == host }
        }
        if (known == null) {
            return DoorReply.Error("no host named $host")
        }
        stores.pairing.open(machine = known)
        if (!stores.pairDigits(pin)) {
            return DoorReply.Error("the code was not six digits")
        }
        return confirmPairing(stores)
    }

    private suspend fun pairByLink(link: String): DoorReply {
        val stores = awaitActiveStores()
            ?: return DoorReply.Error("nothing is running")
        if (!stores.pairLink(link)) {
            return DoorReply.Error("nothing is running")
        }
        return confirmPairing(stores)
    }

    private suspend fun openUrl(url: String): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        if (comp.handleIncomingUri(Uri.parse(url)) == null) {
            return DoorReply.Error("unrecognized link")
        }
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun triggerReport(): DoorReply {
        val activity = activityRef?.get() as? MainActivity
            ?: return DoorReply.Error("no MainActivity on screen")
        activity.screenCaptureCallback.onScreenCaptured()
        awaitFrame()
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun setBackground(backgrounded: Boolean): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        if (backgrounded) {
            comp.runtime.setActive(false)
            comp.runtime.suspending()
        } else {
            comp.runtime.setActive(true)
        }
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun confirmPairing(stores: StoreBundle): DoorReply {
        val checkingResult = awaitPairingPhase(stores.pairing)
        val pending = (checkingResult as? PairingStore.Phase.Confirming)?.pending
            ?: return DoorReply.Error(
                "the pairing was refused: ${stores.pairing.snapshot.refusal ?: checkingResult}",
            )
        stores.confirmPairing(pending)
        val confirmed = awaitPairingPhase(stores.pairing)
        val trusted = confirmed as? PairingStore.Phase.Trusted
            ?: return DoorReply.Error(
                "the machine was not trusted: ${stores.pairing.snapshot.refusal ?: confirmed}",
            )
        return DoorReply.Paired(host = trusted.name)
    }

    private suspend fun awaitPairingPhase(pairing: PairingStore): PairingStore.Phase {
        val deadline = SystemClock.elapsedRealtime() + 30_000L
        while (pairing.snapshot.phase == PairingStore.Phase.Checking &&
            SystemClock.elapsedRealtime() < deadline
        ) {
            delay(50L)
        }
        return pairing.snapshot.phase
    }

    private suspend fun awaitActiveStores(timeoutMs: Long = 30_000L): StoreBundle? {
        val comp = composition ?: return null
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        while (SystemClock.elapsedRealtime() < deadline) {
            val current = comp.runtime.stores
            if (current.profile != null) return current
            delay(50L)
        }
        return comp.runtime.stores.takeIf { it.profile != null }
    }

    private suspend fun connect(
        relay: String,
        token: String,
        user: String,
        tier: String,
    ): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val account = AccountId(user)
        val result = comp.runtime.bind(
            account = account,
            cloudUrl = relay,
            clientId = "cli",
            refreshToken = token,
        )
        if (result.isFailure) {
            return DoorReply.Error(result.exceptionOrNull()?.message ?: "bind failed")
        }
        val entitlement = if (tier.equals("free", ignoreCase = true)) {
            Entitlement.None
        } else {
            Entitlement.Active(grant = Grant.Purchased(EntitlementSource.Web), renewsAtMs = null)
        }
        comp.accounts.entitlement(entitlement, forAccount = account)
        awaitFrame()
        return DoorReply.Ack
    }

    private fun open(screen: String, fixture: String?): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val router = comp.router
        val stores = comp.runtime.stores
        val agent = fixture?.let { named ->
            stores.fleet.snapshot.rows.firstOrNull { row ->
                row.name == named || row.card.agent.toString() == named
            }
        }
        val host = fixture?.let { named ->
            (stores.hosts.snapshot.hosts + stores.hosts.snapshot.discovered).firstOrNull { h ->
                h.name == named || h.id?.toString() == named
            }
        }
        when (screen) {
            "home", "agents" -> {
                router.openFleet()
            }
            "hosts", "computers" -> {
                router.selectTab(Tab.Hosts)
                router.popToRoot()
            }
            "settings", "you" -> router.push(Route.Settings)
            "new-agent" -> router.push(Route.NewAgent)
            "settled" -> router.push(Route.Settled)
            "paywall" -> router.push(Route.Paywall(router.snapshot.tab))
            "pin" -> {
                val id = host?.id ?: return DoorReply.Error("no host named ${fixture.orEmpty()}")
                stores.pairing.open(machine = host)
                router.presentPairing(PairingModal.Code(id))
            }
            "host" -> {
                val id = host?.id ?: return DoorReply.Error("no host named ${fixture.orEmpty()}")
                router.push(Route.Host(id))
            }
            "conversation" -> {
                val row = agent ?: return DoorReply.Error("no agent named ${fixture.orEmpty()}")
                router.push(Route.Conversation(row.card.agent))
            }
            else -> return DoorReply.Error("the running app has no page named $screen")
        }
        return DoorReply.Ack
    }

    private suspend fun setAppearance(name: String): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val mode = when (name.lowercase()) {
            "light" -> AppearanceMode.Light
            "dark" -> AppearanceMode.Dark
            "system" -> AppearanceMode.System
            else -> return DoorReply.Error("no appearance named $name")
        }
        comp.setAppearanceMode(mode)
        awaitFrame()
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun setFontScale(scale: Float): DoorReply {
        if (scale <= 0f || scale > 4f) {
            return DoorReply.Error("invalid fontScale: $scale")
        }
        _fontScaleOverride.value = scale
        awaitFrame()
        awaitFrame()
        return DoorReply.Ack
    }

    private suspend fun awaitReconciled(seconds: Double): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val deadline = SystemClock.elapsedRealtime() + (seconds * 1000.0).toLong()
        while (SystemClock.elapsedRealtime() < deadline) {
            if (comp.runtime.stores.applied.value > 0) return DoorReply.Ack
            delay(50L)
        }
        return DoorReply.Error("never saw the fleet reconciled")
    }

    private suspend fun awaitAgent(agent: String, seconds: Double): DoorReply {
        val comp = composition ?: return DoorReply.Error("app has not started")
        val deadline = SystemClock.elapsedRealtime() + (seconds * 1000.0).toLong()
        while (SystemClock.elapsedRealtime() < deadline) {
            val found = comp.runtime.stores.fleet.snapshot.rows.any { row ->
                row.name == agent || row.card.agent.toString() == agent
            }
            if (found) return DoorReply.Ack
            delay(50L)
        }
        return DoorReply.Error("never saw agent $agent in the fleet")
    }

    private fun currentScreenName(comp: Composition?): String {
        val routerState = comp?.router?.snapshot ?: return "none"
        if (routerState.showsPairing) {
            return when (routerState.pairing) {
                is PairingModal.Code -> "pin"
                is PairingModal.Link -> "pair-link"
                null -> "pairing"
            }
        }
        if (routerState.accountSheetOpen) return "accounts"
        return when (routerState.topRoute) {
            null -> when (routerState.tab) {
                Tab.Agents -> "agents"
                Tab.Hosts -> "hosts"
            }
            is Route.Conversation -> "conversation"
            is Route.Host -> "host"
            Route.Settled -> "settled"
            Route.Settings -> "settings"
            Route.NewAgent -> "new-agent"
            is Route.Paywall -> "paywall"
        }
    }

    // --- Frame settling & screen capture -----------------------------------

    private suspend fun settle() {
        val deadline = SystemClock.elapsedRealtime() + 2_000L
        awaitFrame()
        awaitFrame()
        var quietFrames = 0
        while (SystemClock.elapsedRealtime() < deadline && quietFrames < 2) {
            val roots = composeRoots()
            val layoutBusy = roots.any { (view, _) -> view.isLayoutRequested }
            if (layoutBusy) {
                quietFrames = 0
            } else {
                quietFrames += 1
            }
            awaitFrame()
        }
    }

    private suspend fun awaitFrame() {
        val next = CompletableDeferred<Unit>()
        Choreographer.getInstance().postFrameCallback {
            next.complete(Unit)
        }
        next.await()
    }

    private suspend fun capture(requestedPath: String?): DoorReply {
        settle()
        val activity = activityRef?.get()
            ?: return DoorReply.Error("no activity on screen")
        val window = activity.window
            ?: return DoorReply.Error("activity has no window")
        val decor = window.decorView
        val width = decor.width
        val height = decor.height
        if (width <= 0 || height <= 0) {
            return DoorReply.Error("window has zero dimensions (${width}x$height)")
        }

        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        val copyResult = CompletableDeferred<Int>()
        try {
            PixelCopy.request(
                window,
                bitmap,
                { code -> copyResult.complete(code) },
                mainHandler,
            )
        } catch (_: Throwable) {
            copyResult.complete(PixelCopy.ERROR_UNKNOWN)
        }
        if (copyResult.await() != PixelCopy.SUCCESS) {
            val canvas = Canvas(bitmap)
            decor.draw(canvas)
        }

        // Composite any overlay windows (ModalBottomSheet, AlertDialog) on top.
        val overlayViews = runCatching { WindowInspector.getGlobalWindowViews() }
            .getOrDefault(emptyList())
            .filter { it !== decor && it.visibility == View.VISIBLE && it.width > 0 && it.height > 0 }
        if (overlayViews.isNotEmpty()) {
            val mainLoc = IntArray(2)
            decor.getLocationOnScreen(mainLoc)
            val overlayLoc = IntArray(2)
            val canvas = Canvas(bitmap)
            for (overlay in overlayViews) {
                overlay.getLocationOnScreen(overlayLoc)
                canvas.save()
                canvas.translate(
                    (overlayLoc[0] - mainLoc[0]).toFloat(),
                    (overlayLoc[1] - mainLoc[1]).toFloat(),
                )
                overlay.draw(canvas)
                canvas.restore()
            }
        }

        val pngBytes = ByteArrayOutputStream().use { out ->
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
            out.toByteArray()
        }
        bitmap.recycle()

        val scale = activity.resources.displayMetrics.density
        if (!requestedPath.isNullOrBlank()) {
            val baseDir = File(activity.cacheDir, "door").canonicalFile
            val raw = File(requestedPath)
            val target = (if (raw.isAbsolute) raw else File(baseDir, requestedPath)).canonicalFile
            if (!target.path.startsWith("${baseDir.path}${File.separator}")) {
                return DoorReply.Error("capture path must remain inside ${baseDir.path}")
            }
            target.parentFile?.mkdirs()
            target.writeBytes(pngBytes)
            return DoorReply.Captured(
                path = target.absolutePath,
                width = width,
                height = height,
                scale = scale,
            )
        }
        val encoded = Base64.getEncoder().encodeToString(pngBytes)
        return DoorReply.Captured(
            path = null,
            width = width,
            height = height,
            scale = scale,
            base64 = encoded,
        )
    }

    /**
     * Proves real Android Conscrypt X25519 + HKDF-SHA256 + ChaCha20-Poly1305
     * HPKE decryption on-device using a Rust-sealed vector from `sealed-pushes.json`.
     */
    private fun hpkeSelfCheck(): DoorReply {
        val rawKey = Base64.getDecoder().decode(HPKE_VECTOR_PRIVATE_KEY_B64)
        val opened = SealedPush.open(HPKE_VECTOR_SEALED_B64, PushPrivateKey(rawKey))
            ?: return DoorReply.Error("Conscrypt HPKE self-check failed to decrypt Rust-sealed vector")
        val ask = opened.notice as? Said.Ask
        val run = ask?.wants as? Wants.Run
        if (opened.host != "11111111-1111-1111-1111-111111111111" ||
            opened.agent != "22222222-2222-2222-2222-222222222222" ||
            opened.name != "refactor-auth" ||
            run?.command != "deploy --prod\n--force" ||
            ask.why != "It ships"
        ) {
            return DoorReply.Error("Conscrypt HPKE self-check decoded unexpected payload: $opened")
        }
        return DoorReply.Ack
    }

    private fun parseHostPort(url: String): Pair<String, Int>? = try {
        val uri = URI(url)
        val host = uri.host?.takeIf { it.isNotBlank() } ?: return null
        val port = when {
            uri.port > 0 -> uri.port
            uri.scheme.equals("http", ignoreCase = true) -> 80
            else -> 443
        }
        host to port
    } catch (_: Throwable) {
        null
    }

    private fun formatCloudCall(call: CloudCall): String = when (call) {
        is CloudCall.SignIn -> when (val intent = call.intent) {
            SignInIntent.Adding -> "signIn select"
            is SignInIntent.Returning -> "signIn hint ${intent.account.email}"
        }
        is CloudCall.HandOver -> "handOver ${call.account.value}"
        is CloudCall.ForgetSession -> "forgetSession ${call.account.value}"
        is CloudCall.EntitlementRead -> "entitlement ${call.account.value}"
        is CloudCall.RecordPurchase -> "recordPurchase ${call.account.value}"
        is CloudCall.RequestDeletion -> "requestDeletion ${call.account.value}"
        is CloudCall.RegisterDevice -> "registerDevice ${call.account.value}"
        is CloudCall.RemoveDevice -> "removeDevice ${call.account.value}"
    }

    private fun encodeError(message: String): String =
        DoorCodec.encodeReply(DoorReply.Error(message))

    private fun JsonObject.optionalString(key: String): String? {
        val el = this[key] ?: return null
        if (el is JsonNull) return null
        val prim = el as? JsonPrimitive ?: return null
        return if (prim.isString) prim.content else prim.contentOrNull
    }

    private fun JsonObject.optionalBoolean(key: String): Boolean? =
        this[key]?.takeUnless { it is JsonNull }?.let { (it as? JsonPrimitive)?.booleanOrNull }

    private const val HPKE_VECTOR_PRIVATE_KEY_B64 = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE="
    private const val HPKE_VECTOR_SEALED_B64 =
        "IlAm5YC0RJf8NVkomvep6peArqUkZdIQs2cVTJSplw89OmKB/cIRPicXjYUnga5HQQagDKsckOQrnwXwvjNaA8/Me5OcvrbnSWLkfcAGB5NfI4/DJQqsGLS65l+aZtmyQeWeYEtg4l82uxx4YO0Iv0EaK9OLeNjrRtjzYSWtEFHciJE1BuPUF/246B5HTf7OWzG2KCSJQ3P7N9U/awtUP+kA591am/gxQel+TKZ7yYQHUA1JYIaSLqkqpHHSNfTZwKcC1WaMc3H2EQByy5UTQcyKQ9nS+ZMWHh017tPqUppdc2tLIXUQ4/gg2K7QU0YIE8BPb6e/Frpa8L/7YW0uYqSYSmE9cy3FBF0KQgrvvDRwmsilVw=="
}
