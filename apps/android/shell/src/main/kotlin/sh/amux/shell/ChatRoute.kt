package sh.amux.shell

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.RememberObserver
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import sh.amux.core.runtime.Bridge
import sh.amux.core.runtime.ChatLease
import sh.amux.core.runtime.ChatModel
import sh.amux.core.runtime.DictationState
import sh.amux.core.runtime.HostId
import sh.amux.core.runtime.PickedAttachment
import sh.amux.core.runtime.RuntimeFailure
import sh.amux.core.runtime.StoreBundle
import sh.amux.core.runtime.failureDescription
import sh.amux.design.AmuxGround
import sh.amux.features.AskAction
import sh.amux.features.AttachChoice
import sh.amux.features.ChatOpenedAttachmentUi
import sh.amux.features.ChatRowCellUi
import sh.amux.features.ChatRowSource
import sh.amux.features.ChatScreen
import sh.amux.features.ChatScreenCallbacks
import sh.amux.features.ChatUnavailableScreen
import sh.amux.features.TestTags
import sh.amux.features.defaultValidateAgentName
import sh.amux.features.validateFormFields
import sh.amux.values.AgentKey
import sh.amux.values.BlobRef
import sh.amux.values.Row
import sh.amux.values.Run
import sh.amux.values.RunCounts
import sh.amux.words.OpenableLink
import sh.amux.words.name

/**
 * Live [ChatRowSource] backed by [ChatModel]. Each row cell subscribes solely
 * to its own `RowCell.state` `StateFlow` so changing one row recomposes only
 * that item in the transcript `LazyColumn`.
 */
internal class ModelChatRowSource(
    private val model: ChatModel,
) : ChatRowSource {
    @Composable
    override fun observeCell(id: String): ChatRowCellUi {
        val cell = model.cell(id)
        val snapshot by cell.state.collectAsState()
        return ChatRowCellUi(
            id = snapshot.id,
            row = snapshot.row,
            revision = snapshot.revision,
            attachmentRevision = snapshot.attachmentRevision,
            openedAgent = snapshot.openedAgent,
            groupState = snapshot.groupState,
        )
    }

    override fun shows(row: Row): Boolean = model.shows(row)

    override fun rowBelow(id: String): Row? = model.rowBelow(id)

    override fun countsOf(run: Run): RunCounts? = model.countsOf(run)

    override fun imageBytes(blob: BlobRef): ByteArray? = model.bytesOf(blob)

    override fun revisions(id: String): Flow<Int> = model.cell(id).state.map { it.revision }
}

/**
 * One route's hold on an agent's chat, a lease taken when remembered and
 * released when forgotten or abandoned, so a composition that never commits
 * leaves no chat held open.
 *
 * A chat already open is leased at once, so coming back to it draws without
 * a blank frame. One that is not opens on the core thread once the route is
 * on screen; until it does, [held] is null and the route draws its ground.
 */
private class ChatHold(
    private val stores: StoreBundle,
    private val agent: AgentKey,
    private val scope: CoroutineScope,
) : RememberObserver {
    var held: Result<ChatLease>? by mutableStateOf(stores.leaseCached(agent)?.let { Result.success(it) })
        private set

    private var opening: Job? = null

    override fun onRemembered() {
        if (held != null) return
        opening = scope.launch {
            held = try {
                Result.success(stores.acquireChat(agent))
            } catch (f: RuntimeFailure) {
                Result.failure(f)
            }
        }
    }

    override fun onForgotten() = release()

    override fun onAbandoned() = release()

    private fun release() {
        // A cancelled open closes what it opened.
        opening?.cancel()
        opening = null
        held?.getOrNull()?.release()
    }
}

@Composable
internal fun rememberChatLease(
    stores: StoreBundle,
    agent: AgentKey,
    retryNonce: Int = 0,
): Result<ChatLease>? {
    val profileReady by stores.profileReady.collectAsState()
    val profileVersion by stores.profileVersion.collectAsState()
    val scope = rememberCoroutineScope()
    val hold = remember(stores, agent, profileReady, profileVersion, retryNonce) {
        ChatHold(stores = stores, agent = agent, scope = scope)
    }
    return hold.held
}

/**
 * An agent's chat. It reads its own fleet row, its computer and its family,
 * each only when that part changes, so the rest of the fleet moving leaves
 * the open chat alone.
 */
@Composable
internal fun ChatRoute(
    agent: AgentKey,
    stores: StoreBundle,
    router: Router,
    dictation: DictationController? = null,
) {
    val context = LocalContext.current
    val coroutineScope = rememberCoroutineScope()

    val opened = rememberChatLease(stores = stores, agent = agent)
    if (opened == null) {
        AmuxGround(modifier = Modifier.fillMaxSize().testTag(TestTags.SCREEN_CONVERSATION)) {}
        return
    }
    val model = opened.getOrNull()?.model
    if (model == null) {
        val name by stores.fleet.state.collectPart(agent) { it.row(agent)?.name }
        ChatUnavailableScreen(
            title = name ?: agent.toString(),
            reason = opened.exceptionOrNull()?.failureDescription ?: "",
            onBack = { router.pop() },
        )
        return
    }

    val currentDictation by rememberUpdatedState(dictation)
    // Asking the recognizer whether it exists binds a system service, so it is
    // asked once per controller rather than on every frame.
    val dictationAvailable = remember(dictation) { dictation?.isAvailable == true }

    val startDictationSession: () -> Unit = remember(model) {
        {
            val controller = currentDictation
            if (controller == null || !controller.isAvailable) {
                model.prepareDictation(
                    speech = DictationState.Permission.Allowed,
                    microphone = DictationState.Permission.Allowed,
                    available = false,
                )
            } else {
                model.prepareDictation(
                    speech = DictationState.Permission.Allowed,
                    microphone = DictationState.Permission.Allowed,
                    available = true,
                )
                model.beginDictation()
                controller.start(
                    onPartial = { text -> model.heard(text) },
                    onFinal = { text ->
                        if (text.isNotEmpty()) {
                            model.heard(text)
                        }
                        model.stopDictation()
                    },
                    onError = { model.dictationFailed() },
                )
            }
        }
    }

    val recordAudioPermissionLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            startDictationSession()
        } else {
            model.prepareDictation(
                speech = DictationState.Permission.Allowed,
                microphone = DictationState.Permission.Denied,
                available = currentDictation?.isAvailable == true,
            )
        }
    }

    DisposableEffect(model, dictation) {
        onDispose {
            dictation?.cancel()
            model.stopDictation()
        }
    }

    val photoPicker = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            coroutineScope.launch(start = CoroutineStart.UNDISPATCHED) {
                ShellPlatform.attachPickedUri(
                    context = context,
                    uri = uri,
                    isPhoto = true,
                    model = model,
                )
            }
        }
    }

    val filePicker = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri != null) {
            coroutineScope.launch(start = CoroutineStart.UNDISPATCHED) {
                ShellPlatform.attachPickedUri(
                    context = context,
                    uri = uri,
                    isPhoto = false,
                    model = model,
                )
            }
        }
    }

    val chatState by model.state.collectAsState()

    // The clock ticks only while something on screen counts time; the screen
    // hands it to the few leaves that print a time, so a tick redraws those.
    val needsClockTick = chatState.frame?.composer?.activity != null ||
        chatState.overview?.jobs?.isNotEmpty() == true
    val clock = rememberTickingClock(now = stores.clock, ticking = needsClockTick)

    val openedFile = chatState.openedAttachment
    var openedAttachmentUi by remember(openedFile) {
        mutableStateOf(
            openedFile?.let { file ->
                ChatOpenedAttachmentUi(name = file.name, bytes = null, isImage = false, file = file)
            },
        )
    }
    LaunchedEffect(openedFile) {
        if (openedFile == null) {
            openedAttachmentUi = null
        } else {
            openedAttachmentUi = withContext(Dispatchers.IO) {
                val head = ByteArray(12)
                val read = runCatching {
                    openedFile.inputStream().use { it.read(head) }
                }.getOrDefault(0)
                val sniffedMime = if (read > 0) {
                    PickedAttachment.sniffNativeImageMime(head.copyOf(read))
                } else {
                    null
                }
                val imageBytes = if (sniffedMime != null) {
                    runCatching { openedFile.readBytes() }.getOrNull()
                } else {
                    null
                }
                ChatOpenedAttachmentUi(
                    name = openedFile.name,
                    bytes = imageBytes,
                    isImage = sniffedMime != null,
                    file = openedFile,
                )
            }
        }
    }

    val fleetRow by stores.fleet.state.collectPart(agent) { it.row(agent) }
    val hostId = remember(agent) { HostId.fromBytes(agent.host) }
    val host by stores.hosts.state.collectPart(hostId) { hosts -> hostId?.let(hosts::host) }
    val connecting by stores.hosts.state.collectPart(hostId) { it.isConnecting(hostId) }
    // The family is read once as the chat opens, so its bar is in the first
    // frame, and followed off the main thread after that.
    val familyAtOpen = remember(stores, agent) { stores.familyNow(agent) }
    val family by remember(stores, agent) { stores.family(agent) }.collectAsState(initial = familyAtOpen)

    val subject = remember(agent, fleetRow, chatState.frame, host, connecting) {
        ChatMappers.toChatSubject(
            agent = agent,
            fleetRow = fleetRow,
            frame = chatState.frame,
            host = host,
            connecting = connecting,
        )
    }
    val rowSource = remember(model) { ModelChatRowSource(model) }
    val screenState = remember(chatState, subject, family, openedAttachmentUi, dictationAvailable) {
        val askKey = chatState.ask?.key
        ChatMappers.toChatScreenState(
            chat = chatState,
            subject = subject,
            family = family,
            keptQuestionDraft = askKey?.let(model::questionDraft),
            keptFormValues = askKey?.let(model::formDraft),
            openedAttachment = openedAttachmentUi,
            dictationEnabled = dictationAvailable,
        )
    }

    val callbacks = remember(model, stores, agent, router, context, dictation) {
        ChatScreenCallbacks(
            onBack = { router.pop() },
            onReadingChange = { atNewest -> model.reading(atNewest) },
            onReachedTop = { firstVisible -> model.reachedTop(firstVisible) },
            onJumpToNewest = { model.jumpToNewest() },
            onToggleExpand = { rowId -> model.toggle(rowId) },
            onToggleRun = { rowId -> model.toggleRun(rowId) },
            onToggleGroup = { rowId -> model.toggleGroup(rowId) },
            onOpenAgent = { target -> router.push(Route.Conversation(target)) },
            onOpenReview = { filePath -> router.push(Route.Changes(agent = agent, initialFile = filePath)) },
            onOpenAttachment = { blob -> model.openAttachment(blob) },
            onOpenAttachmentExternally = { file -> ShellPlatform.openOrShareFile(context, file) },
            onShareAttachment = { file -> ShellPlatform.shareFile(context, file) },
            onClearOpenedAttachment = { model.clearOpenedAttachment() },
            onOpenLink = { link -> ShellPlatform.openExternalLink(context, link) },
            onCopyCode = { code -> ShellPlatform.copyToClipboard(context, code) },
            onCopyAddress = { address -> ShellPlatform.copyToClipboard(context, address) },
            onDraftChange = { text -> model.type(text) },
            onSend = {
                dictation?.cancel()
                model.stopDictation()
                model.send()
            },
            onResume = {
                dictation?.cancel()
                model.stopDictation()
                model.resume()
            },
            onInterrupt = { model.interrupt() },
            onAttach = { choice ->
                when (choice) {
                    AttachChoice.Photo -> {
                        photoPicker.launch(
                            PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                        )
                    }
                    AttachChoice.File -> {
                        filePicker.launch(arrayOf("*/*"))
                    }
                }
            },
            onRemoveAttachment = { index -> model.removeAttachment(index) },
            onPickCommand = { match -> model.pick(match) },
            onSendNow = { queued -> model.sendNow(queued) },
            onWithdraw = { queued -> model.withdraw(queued) },
            onResend = { inputId -> model.resend(inputId) },
            onDiscard = { inputId -> model.discard(inputId) },
            onDismissRefused = { inputId -> model.discard(inputId) },
            onSwitchBack = { model.switchBack() },
            onStopJob = { key -> model.stopJob(key) },
            onToggleDictation = {
                val controller = dictation
                if (model.state.value.dictation.active) {
                    if (controller != null) {
                        controller.stop()
                    } else {
                        model.stopDictation()
                    }
                } else if (controller != null && controller.isAvailable) {
                    val hasMicPermission = ContextCompat.checkSelfPermission(
                        context,
                        Manifest.permission.RECORD_AUDIO,
                    ) == PackageManager.PERMISSION_GRANTED
                    if (hasMicPermission) {
                        startDictationSession()
                    } else {
                        model.prepareDictation(
                            speech = DictationState.Permission.Allowed,
                            microphone = DictationState.Permission.NotAsked,
                            available = true,
                        )
                        recordAudioPermissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                    }
                } else {
                    model.prepareDictation(
                        speech = DictationState.Permission.Allowed,
                        microphone = DictationState.Permission.Allowed,
                        available = false,
                    )
                }
            },
            onAskAction = { action ->
                when (action) {
                    is AskAction.Choose -> model.answer(
                        choice = action.index,
                        note = action.note,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Respond -> model.answerQuestions(
                        responses = action.responses,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Reply -> model.replyInstead(
                        text = action.text,
                        soFar = action.soFar,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.Submit -> model.submitForm(
                        values = action.values,
                        expectedKey = action.askKey.ifEmpty { null },
                    )
                    is AskAction.OpenUrl -> {
                        OpenableLink.parse(action.url)?.let { link ->
                            ShellPlatform.openExternalLink(context, link)
                        }
                    }
                    AskAction.Interrupt -> model.interrupt()
                    AskAction.Resend -> model.resendAnswer()
                    AskAction.Discard -> model.discardAnswer()
                }
            },
            onQuestionDraftChange = { askKey, uiDraft ->
                model.keepQuestionDraft(askKey, ChatMappers.toRuntimeQuestionDraft(uiDraft))
            },
            onFormValuesChange = { askKey, values ->
                model.keepFormDraft(askKey, values)
            },
            validateForm = { fields, values ->
                runCatching { model.formProblems(fields, values) }
                    .getOrElse { validateFormFields(fields, values) }
            },
            onSelectComparison = { comparison -> model.compare(comparison) },
            onRetryChangedFiles = { model.retryChangedFiles() },
            onChangeSetting = { setting -> model.change(setting) },
            onOverviewOpenChange = { shown -> model.showOverview(shown) },
            onClearRequestedOverlay = { model.clearAsk() },
            onPin = { coroutineScope.launch { stores.pin(agent) } },
            onUnpin = { coroutineScope.launch { stores.unfile(agent) } },
            onRename = { newName -> coroutineScope.launch { stores.rename(newName, agent) } },
            onSettle = { coroutineScope.launch { stores.close(agent) } },
            onUnsettle = { coroutineScope.launch { stores.unfile(agent) } },
            onCancelSettle = { coroutineScope.launch { stores.cancelClose(agent) } },
            onDelete = { discardingChanges ->
                coroutineScope.launch {
                    val outcome = stores.delete(agent, discardingChanges)
                    if (outcome.isSuccess) {
                        router.pop()
                    }
                }
            },
            onPreviewDelete = { stores.deleteWarning(agent) },
            validateAgentName = ::validateAgentName,
        )
    }

    ChatScreen(
        state = screenState,
        rowSource = rowSource,
        callbacks = callbacks,
        clock = clock,
    )
}

/** The core's check of a new agent name, or the local one when the core is not loaded. */
internal fun validateAgentName(candidate: String) =
    runCatching { Bridge.agentNameProblem(candidate) }
        .getOrElse { defaultValidateAgentName(candidate) }
