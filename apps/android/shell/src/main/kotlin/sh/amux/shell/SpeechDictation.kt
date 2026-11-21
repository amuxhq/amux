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

    fun stop()

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
 * Main looper, requesting partial hypotheses and cleaning up recognizer state
 * safely across errors, stops, and destruction.
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
            val current = activeSession
            activeSession = null
            if (current != null) {
                current.active = false
            }
            recognizer?.let { backend ->
                runCatching { backend.stopListening() }
                runCatching { backend.cancel() }
            }
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
            session.active = false
            activeSession = null
            // Reset the recognizer after an error so a subsequent start() recovers cleanly.
            tearDownRecognizer()
            session.onError()
        }

        override fun onResults(results: Bundle?) {
            if (!session.active || activeSession !== session) return
            session.active = false
            activeSession = null
            val text = extractTranscript(results)
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
