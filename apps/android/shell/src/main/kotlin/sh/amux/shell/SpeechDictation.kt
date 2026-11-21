package sh.amux.shell

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer

/**
 * Speech-to-text dictation controller used by `:shell` to feed live partial
 * and final hypotheses into [sh.amux.core.runtime.ChatModel].
 */
interface DictationController {
    val isAvailable: Boolean

    fun start(
        onPartial: (String) -> Unit,
        onFinal: (String) -> Unit,
        onError: () -> Unit,
    )

    /**
     * User-initiated stop: tells the recognizer to stop capturing audio and
     * wait for its trailing final result before releasing the recognizer.
     */
    fun stop()

    /**
     * Immediate cancellation: discards any pending utterance and destroys the
     * recognizer immediately (used when leaving chat or backgrounding).
     */
    fun cancel() {
        stop()
    }

    fun destroy()
}

/** Internal seam around [SpeechRecognizer] so [AndroidSpeechDictation] is unit-testable. */
internal interface SpeechRecognizerBackend {
    fun setRecognitionListener(listener: RecognitionListener)
    fun startListening(intent: Intent)
    fun stopListening()
    fun cancel()
    fun destroy()
}

private class PlatformSpeechRecognizerBackend(
    private val delegate: SpeechRecognizer,
) : SpeechRecognizerBackend {
    override fun setRecognitionListener(listener: RecognitionListener) {
        delegate.setRecognitionListener(listener)
    }

    override fun startListening(intent: Intent) {
        delegate.startListening(intent)
    }

    override fun stopListening() {
        delegate.stopListening()
    }

    override fun cancel() {
        delegate.cancel()
    }

    override fun destroy() {
        delegate.destroy()
    }
}

/**
 * Android [DictationController] wrapping [SpeechRecognizer] strictly on the
 * Main looper, requesting partial hypotheses, letting user-initiated [stop]
 * deliver trailing final utterances, and tearing down cleanly on [cancel],
 * errors, and [destroy].
 */
class AndroidSpeechDictation internal constructor(
    private val context: Context,
    private val mainHandler: Handler,
    private val availabilityCheck: (Context) -> Boolean,
    private val recognizerFactory: (Context) -> SpeechRecognizerBackend,
) : DictationController {

    constructor(context: Context) : this(
        context = context.applicationContext,
        mainHandler = Handler(Looper.getMainLooper()),
        availabilityCheck = { SpeechRecognizer.isRecognitionAvailable(it) },
        recognizerFactory = { PlatformSpeechRecognizerBackend(SpeechRecognizer.createSpeechRecognizer(it)) },
    )

    private class Session(
        val onPartial: (String) -> Unit,
        val onFinal: (String) -> Unit,
        val onError: () -> Unit,
        var active: Boolean = true,
        var stopping: Boolean = false,
    )

    private var recognizer: SpeechRecognizerBackend? = null
    private var activeSession: Session? = null
    private var destroyed: Boolean = false

    override val isAvailable: Boolean
        get() = runCatching { availabilityCheck(context) }.getOrDefault(false)

    override fun start(
        onPartial: (String) -> Unit,
        onFinal: (String) -> Unit,
        onError: () -> Unit,
    ) {
        runOnMain {
            if (destroyed || !isAvailable) {
                onError()
                return@runOnMain
            }

            // End any prior session before beginning a new one.
            activeSession?.active = false
            activeSession = null
            recognizer?.let { runCatching { it.cancel() } }

            val session = Session(
                onPartial = onPartial,
                onFinal = onFinal,
                onError = onError,
            )
            activeSession = session

            val backend = recognizer ?: runCatching { recognizerFactory(context) }.getOrNull()
            if (backend == null) {
                session.active = false
                activeSession = null
                onError()
                return@runOnMain
            }
            recognizer = backend

            backend.setRecognitionListener(createListener(session))
            val intent = buildRecognizerIntent(context.packageName)
            val started = runCatching { backend.startListening(intent) }.isSuccess
            if (!started) {
                session.active = false
                if (activeSession === session) {
                    activeSession = null
                }
                tearDownRecognizer()
                onError()
            }
        }
    }

    override fun stop() {
        runOnMain {
            val current = activeSession ?: return@runOnMain
            if (!current.active) return@runOnMain
            if (current.stopping) {
                // Second stop tap while waiting for trailing results: force finish cleanly.
                current.active = false
                activeSession = null
                tearDownRecognizer()
                current.onFinal("")
                return@runOnMain
            }
            current.stopping = true
            val backend = recognizer
            if (backend == null) {
                current.active = false
                activeSession = null
                current.onFinal("")
                return@runOnMain
            }
            val stopped = runCatching { backend.stopListening() }.isSuccess
            if (!stopped) {
                current.active = false
                activeSession = null
                tearDownRecognizer()
                current.onFinal("")
            }
        }
    }

    override fun cancel() {
        runOnMain {
            val current = activeSession
            activeSession = null
            if (current != null) {
                current.active = false
            }
            tearDownRecognizer()
        }
    }

    override fun destroy() {
        runOnMain {
            if (destroyed && recognizer == null && activeSession == null) {
                return@runOnMain
            }
            destroyed = true
            activeSession?.active = false
            activeSession = null
            tearDownRecognizer()
        }
    }

    private fun tearDownRecognizer() {
        val current = recognizer
        recognizer = null
        if (current != null) {
            runCatching { current.cancel() }
            runCatching { current.destroy() }
        }
    }

    private fun createListener(session: Session): RecognitionListener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) = Unit

        override fun onBeginningOfSpeech() = Unit

        override fun onRmsChanged(rmsdB: Float) = Unit

        override fun onBufferReceived(buffer: ByteArray?) = Unit

        override fun onEndOfSpeech() = Unit

        override fun onError(error: Int) {
            if (!session.active || activeSession !== session) return
            val wasStopping = session.stopping
            session.active = false
            activeSession = null
            // Reset the recognizer after an error so a subsequent start() recovers cleanly.
            tearDownRecognizer()
            if (wasStopping && isBenignStopError(error)) {
                session.onFinal("")
            } else {
                session.onError()
            }
        }

        override fun onResults(results: Bundle?) {
            if (!session.active || activeSession !== session) return
            session.active = false
            activeSession = null
            val text = extractTranscript(results)
            tearDownRecognizer()
            session.onFinal(text)
        }

        override fun onPartialResults(partialResults: Bundle?) {
            if (!session.active || activeSession !== session) return
            val text = extractTranscript(partialResults)
            if (text.isNotEmpty()) {
                session.onPartial(text)
            }
        }

        override fun onEvent(eventType: Int, params: Bundle?) = Unit
    }

    private fun runOnMain(block: () -> Unit) {
        if (Looper.myLooper() == mainHandler.looper) {
            block()
        } else {
            mainHandler.post(block)
        }
    }

    companion object {
        private fun isBenignStopError(error: Int): Boolean =
            error == SpeechRecognizer.ERROR_NO_MATCH ||
                error == SpeechRecognizer.ERROR_SPEECH_TIMEOUT ||
                error == SpeechRecognizer.ERROR_CLIENT

        internal fun buildRecognizerIntent(callingPackage: String): Intent =
            Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(
                    RecognizerIntent.EXTRA_LANGUAGE_MODEL,
                    RecognizerIntent.LANGUAGE_MODEL_FREE_FORM,
                )
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)
                putExtra(RecognizerIntent.EXTRA_CALLING_PACKAGE, callingPackage)
            }

        private fun extractTranscript(bundle: Bundle?): String =
            bundle
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
                ?.trim()
                .orEmpty()
    }
}
