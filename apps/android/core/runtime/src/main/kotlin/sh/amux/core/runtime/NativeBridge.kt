package sh.amux.core.runtime

/**
 * Low-level JNI declarations backed by `crates/app-jni` (`libamux_app`).
 *
 * Host JVM tests may pass `-Damux.jni.lib=path/to/libamux_app.so` (or `.dylib`)
 * so the real Rust library loads without Android packaging; on Android,
 * `System.loadLibrary("amux_app")` loads the bundled ABI library. The path may
 * be relative to the working directory, as the build gives it.
 */
object NativeBridge {
    init {
        val explicit = System.getProperty("amux.jni.lib")?.takeIf { it.isNotBlank() }
        if (explicit != null) {
            System.load(java.io.File(explicit).absolutePath)
        } else {
            System.loadLibrary("amux_app")
        }
    }

    // --- stateless helpers -------------------------------------------------

    @JvmStatic external fun version(): String

    @JvmStatic external fun agentNameProblem(name: String): String?

    @JvmStatic external fun newAgentSettings(catalogueJson: String, chosenJson: String): String?

    @JvmStatic external fun newAgentPick(
        catalogueJson: String,
        chosenJson: String,
        pickJson: String,
    ): String?

    @JvmStatic external fun onboardingNext(
        onboardingJson: String,
        afterJson: String?,
        finishing: Boolean,
    ): String?

    @JvmStatic external fun formProblems(fieldsJson: String, valuesJson: String): String?

    @JvmStatic external fun commandMatches(commandsJson: String, draft: String): String?

    @JvmStatic external fun pastesAsAttachment(pasted: String): Boolean

    @JvmStatic external fun attachmentLimit(): Long

    /**
     * A fenced code block coloured as the shared view colours it: a
     * `HighlightedCode` as JSON. One not `settled` is drawn as it is and
     * asked for again within a tenth of a second.
     */
    @JvmStatic external fun highlightBlock(code: String, info: String?, streaming: Boolean): String?

    /** An edit's hunks (`[PatchHunk]` as JSON) coloured as the file at `path` is. */
    @JvmStatic external fun highlightPatch(path: String, hunksJson: String): String?

    // --- runtime lifecycle & stepping --------------------------------------

    @JvmStatic external fun runtimeStart(configJson: String): Long

    @JvmStatic external fun runtimeStop(handle: Long)

    /**
     * Waits on the runtime's wake descriptor via POSIX `poll`.
     * Returns `1` when readable, `0` on timeout/EINTR, or `-1` once the
     * runtime is stopping ([runtimeInterruptWake]) or stopped.
     */
    @JvmStatic external fun runtimeWaitWake(handle: Long, timeoutMs: Int): Int

    /**
     * Marks the runtime as stopping: a thread in [runtimeWaitWake] returns
     * `-1`, and so does every wait after, whatever a step reads meanwhile.
     */
    @JvmStatic external fun runtimeInterruptWake(handle: Long)

    @JvmStatic external fun runtimeStep(handle: Long): String?

    @JvmStatic external fun runtimeFeed(handle: Long, lifecycleJson: String)

    // --- profile registry & installation acts ------------------------------

    @JvmStatic external fun runtimeCreateProfile(handle: Long): Long

    @JvmStatic external fun runtimeDeleteProfile(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeBind(
        handle: Long,
        profileId: String?,
        cloudUrl: String,
        clientId: String,
        refreshToken: String,
    ): Long

    @JvmStatic external fun runtimeSignOut(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeTrustingProfiles(handle: Long, hostIdJson: String): Long

    @JvmStatic external fun runtimeOfferPairing(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeListSources(handle: Long, profileId: String?)

    @JvmStatic external fun runtimeSourcePolicyListed(handle: Long, profileId: String): Boolean

    @JvmStatic external fun runtimeDiscovered(handle: Long, foundJson: String)

    @JvmStatic external fun runtimeAccessToken(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeHostId(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeWantNotifications(
        handle: Long,
        profileId: String,
        keyJson: String,
    ): Long

    @JvmStatic external fun runtimeRefreshEntitlement(handle: Long, profileId: String): Long

    @JvmStatic external fun runtimeSetTelemetry(handle: Long, on: Boolean)

    @JvmStatic external fun runtimeSetAutoReports(handle: Long, on: Boolean)

    @JvmStatic external fun runtimeRecord(handle: Long, profileId: String?, eventJson: String)

    @JvmStatic external fun runtimeSettings(handle: Long): Long

    @JvmStatic external fun runtimeUpdateSettings(handle: Long, changeJson: String): Long

    @JvmStatic external fun runtimeSuggestRepositories(handle: Long): Long

    @JvmStatic external fun runtimeWebSignInLink(
        handle: Long,
        profileId: String,
        path: String,
    ): Long

    // --- profiles & fleet --------------------------------------------------

    @JvmStatic external fun profileOpen(runtimeHandle: Long, profileId: String): Long

    @JvmStatic external fun profileClose(handle: Long)

    @JvmStatic external fun profileFleetId(handle: Long): Long

    @JvmStatic external fun profileDump(handle: Long, reason: String): Long

    @JvmStatic external fun profileSubmitReport(
        handle: Long,
        dumpPath: String,
        header: ByteArray,
        frame: ByteArray,
    ): Long

    @JvmStatic external fun profileBeginPair(handle: Long, requestJson: String): Long

    @JvmStatic external fun profileOfferSignIn(handle: Long, hostIdJson: String): Long

    @JvmStatic external fun profileAnswerSignInOffer(handle: Long, answerJson: String): Long

    @JvmStatic external fun profileWatchOnboarding(handle: Long)

    @JvmStatic external fun profileAnswerOnboarding(handle: Long, answerJson: String): Long

    @JvmStatic external fun profileOpenPairing(handle: Long, ttlSeconds: Long): Long

    @JvmStatic external fun profileClosePairing(handle: Long): Long

    @JvmStatic external fun profileConfirmPair(handle: Long, tokenJson: String): Long

    @JvmStatic external fun profileAbandonPair(handle: Long, tokenJson: String): Long

    @JvmStatic external fun profileUnpair(handle: Long, hostIdJson: String): Long

    @JvmStatic external fun profileRoster(handle: Long): Long

    @JvmStatic external fun profileCreateAgent(handle: Long, agentJson: String): Long

    @JvmStatic external fun profileDirectories(
        handle: Long,
        hostIdJson: String,
        query: String,
        limit: Int,
    ): Long

    @JvmStatic external fun profilePinFolder(
        handle: Long,
        hostIdJson: String,
        path: String,
        pinned: Boolean,
    ): Long

    @JvmStatic external fun profileAgentAct(
        handle: Long,
        agentJson: String,
        actJson: String,
    ): Long

    @JvmStatic external fun profileDeleteWarning(handle: Long, agentJson: String): Long

    @JvmStatic external fun profileHostCatalogue(
        handle: Long,
        hostIdJson: String,
        providerJson: String,
    ): Long

    @JvmStatic external fun profileSetForeground(handle: Long, foreground: Boolean)

    @JvmStatic external fun fleetOpening(handle: Long): String?

    @JvmStatic external fun fleetToggleFamily(handle: Long, rootJson: String)

    @JvmStatic external fun fleetFilter(handle: Long, filterJson: String)

    @JvmStatic external fun fleetCard(handle: Long, agentJson: String): String?

    @JvmStatic external fun fleetFamily(handle: Long, agentJson: String): String?

    // --- sessions / chats --------------------------------------------------

    @JvmStatic external fun sessionOpen(profileHandle: Long, agentJson: String, tail: Int): Long

    @JvmStatic external fun sessionId(handle: Long): Long

    @JvmStatic external fun sessionClose(handle: Long)

    @JvmStatic external fun sessionOpening(handle: Long): String?

    @JvmStatic external fun sessionFollow(handle: Long, following: Boolean)

    @JvmStatic external fun sessionKeptDraft(handle: Long): String?

    @JvmStatic external fun sessionKeepDraft(handle: Long, draftJson: String)

    @JvmStatic external fun sessionRowsFor(handle: Long, keysJson: String): String?

    @JvmStatic external fun sessionToggleRun(handle: Long, member: String)

    @JvmStatic external fun sessionToggleGroup(handle: Long, member: String)

    @JvmStatic external fun sessionFoldTurns(handle: Long, on: Boolean)

    @JvmStatic external fun sessionToggleTurn(handle: Long, end: String)

    @JvmStatic external fun sessionAct(handle: Long, actJson: String): Long

    @JvmStatic external fun sessionDraftOf(handle: Long, inputIdJson: String): String?

    @JvmStatic external fun sessionDiscard(handle: Long, inputIdJson: String)

    @JvmStatic external fun sessionOpenOverview(handle: Long, comparisonJson: String): Long

    // --- the review page open on a chat -------------------------------------

    /** Opens the agent's review page on the chat; the ticket answers a `ReviewOutcome`. */
    @JvmStatic external fun reviewOpen(handle: Long, baseBranch: String?, refresh: Boolean): Long

    /** Opens a sent review's round by its `SentReviewRef`; the ticket answers a `ReviewPage` or null. */
    @JvmStatic external fun reviewRound(handle: Long, sentJson: String): Long

    /** Fetches file bodies (`[String]` as JSON, or null for every file in `to_fetch`). */
    @JvmStatic external fun reviewFiles(handle: Long, pathsJson: String?): Long

    /** Loads more context for a gap's `ReviewContextAsk`. */
    @JvmStatic external fun reviewContext(handle: Long, askJson: String): Long

    /** Changes the review at the agent's host: `[ReviewChange]`, all or none. */
    @JvmStatic external fun reviewEdit(handle: Long, changesJson: String): Long

    /** Takes a `ReviewChoice`; the next step hands over the page drawn so. */
    @JvmStatic external fun reviewChoose(handle: Long, choiceJson: String)

    /** The open review page as a `ReviewPage`, or JSON null with none open. */
    @JvmStatic external fun reviewPage(handle: Long): String?

    /** The `ReviewAnchor` a selection of the last page's rows comments on, or JSON null. */
    @JvmStatic external fun reviewAnchor(handle: Long, from: Int, to: Int): String?

    /** Closes the review page open on the chat. */
    @JvmStatic external fun reviewClose(handle: Long)

    @JvmStatic external fun sessionPutBlob(
        handle: Long,
        bytes: ByteArray,
        name: String,
        mime: String,
    ): Long

    @JvmStatic external fun sessionBlob(handle: Long, hashJson: String): ByteArray?

    @JvmStatic external fun sessionBlobFailed(handle: Long, hashJson: String): Boolean

    @JvmStatic external fun sessionFetchBlob(handle: Long, hashJson: String)
}
