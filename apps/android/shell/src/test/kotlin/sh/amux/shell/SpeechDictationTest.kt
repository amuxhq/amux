package sh.amux.shell

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SpeechDictationTest {
    private class FakeBackend : SpeechRecognizerBackend {
        var listener: RecognitionListener? = null
        var startedIntent: Intent? = null
        var stopCount: Int = 0
        var cancelCount: Int = 0
        var destroyCount: Int = 0

        override fun setRecognitionListener(listener: RecognitionListener) {
            this.listener = listener
        }

        override fun startListening(intent: Intent) {
            this.startedIntent = intent
        }

        override fun stopListening() {
            stopCount += 1
        }

        override fun cancel() {
            cancelCount += 1
        }

        override fun destroy() {
            destroyCount += 1
        }
    }

    private fun resultsBundle(vararg phrases: String): Bundle = Bundle().apply {
        putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, ArrayList(phrases.toList()))
    }

    @Test
    fun deliversPartialAndFinalHypothesesWithExpectedRecognizerIntent() {
        val context = RuntimeEnvironment.getApplication()
        val backend = FakeBackend()
        val dictation = AndroidSpeechDictation(
            context = context,
            mainHandler = Handler(Looper.getMainLooper()),
            availabilityCheck = { true },
            recognizerFactory = { backend },
        )

        assertTrue(dictation.isAvailable)

        val partials = mutableListOf<String>()
        val finals = mutableListOf<String>()
        var errors = 0

        dictation.start(
            onPartial = { partials += it },
            onFinal = { finals += it },
            onError = { errors += 1 },
        )

        val intent = backend.startedIntent
        assertNotNull(intent)
        assertEquals(RecognizerIntent.ACTION_RECOGNIZE_SPEECH, intent!!.action)
        assertEquals(
            RecognizerIntent.LANGUAGE_MODEL_FREE_FORM,
            intent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL),
        )
        assertTrue(intent.getBooleanExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false))
        assertEquals(context.packageName, intent.getStringExtra(RecognizerIntent.EXTRA_CALLING_PACKAGE))

        val listener = backend.listener!!
        listener.onPartialResults(resultsBundle("hello"))
        listener.onPartialResults(resultsBundle("hello world"))
        listener.onResults(resultsBundle("hello world today"))

        assertEquals(listOf("hello", "hello world"), partials)
        assertEquals(listOf("hello world today"), finals)
        assertEquals(0, errors)

        // Late callbacks after completion are ignored.
        listener.onPartialResults(resultsBundle("stale"))
        listener.onError(SpeechRecognizer.ERROR_NO_MATCH)
        assertEquals(listOf("hello", "hello world"), partials)
        assertEquals(0, errors)
    }

    @Test
    fun errorRecoversWithFreshRecognizerOnNextStart() {
        val context = RuntimeEnvironment.getApplication()
        val created = mutableListOf<FakeBackend>()
        val dictation = AndroidSpeechDictation(
            context = context,
            mainHandler = Handler(Looper.getMainLooper()),
            availabilityCheck = { true },
            recognizerFactory = { FakeBackend().also { created += it } },
        )

        var errors = 0
        val finals = mutableListOf<String>()
        dictation.start(onPartial = {}, onFinal = { finals += it }, onError = { errors += 1 })
        assertEquals(1, created.size)

        created[0].listener!!.onError(SpeechRecognizer.ERROR_AUDIO)
        assertEquals(1, errors)
        assertEquals(1, created[0].destroyCount)

        dictation.start(onPartial = {}, onFinal = { finals += it }, onError = { errors += 1 })
        assertEquals(2, created.size)
        created[1].listener!!.onResults(resultsBundle("recovered"))
        assertEquals(listOf("recovered"), finals)
        assertEquals(1, errors)
    }

    @Test
    fun stopAndDestroyAreIdempotentAndSuppressLateCallbacks() {
        val context = RuntimeEnvironment.getApplication()
        val backend = FakeBackend()
        val dictation = AndroidSpeechDictation(
            context = context,
            mainHandler = Handler(Looper.getMainLooper()),
            availabilityCheck = { true },
            recognizerFactory = { backend },
        )

        val partials = mutableListOf<String>()
        val finals = mutableListOf<String>()
        var errors = 0

        dictation.start(
            onPartial = { partials += it },
            onFinal = { finals += it },
            onError = { errors += 1 },
        )

        dictation.stop()
        dictation.stop()

        backend.listener!!.onPartialResults(resultsBundle("after stop"))
        backend.listener!!.onResults(resultsBundle("final after stop"))
        backend.listener!!.onError(SpeechRecognizer.ERROR_CLIENT)
        assertTrue(partials.isEmpty())
        assertTrue(finals.isEmpty())
        assertEquals(0, errors)

        dictation.destroy()
        dictation.destroy()
        assertEquals(1, backend.destroyCount)

        // Starting after destroy reports an error without creating a new session.
        dictation.start(onPartial = {}, onFinal = {}, onError = { errors += 1 })
        assertEquals(1, errors)
    }

    @Test
    fun unavailableRecognizerCallsOnErrorWithoutCreatingBackend() {
        val context = RuntimeEnvironment.getApplication()
        var factoryCalls = 0
        val dictation = AndroidSpeechDictation(
            context = context,
            mainHandler = Handler(Looper.getMainLooper()),
            availabilityCheck = { false },
            recognizerFactory = {
                factoryCalls += 1
                FakeBackend()
            },
        )

        assertFalse(dictation.isAvailable)
        var errors = 0
        dictation.start(onPartial = {}, onFinal = {}, onError = { errors += 1 })
        assertEquals(1, errors)
        assertEquals(0, factoryCalls)
    }

    @Test
    fun backgroundThreadCallsDispatchOntoMainLooper() {
        val context = RuntimeEnvironment.getApplication()
        val backend = FakeBackend()
        val dictation = AndroidSpeechDictation(
            context = context,
            mainHandler = Handler(Looper.getMainLooper()),
            availabilityCheck = { true },
            recognizerFactory = { backend },
        )

        val worker = Thread {
            dictation.start(onPartial = {}, onFinal = {}, onError = {})
        }
        worker.start()
        worker.join()

        shadowOf(Looper.getMainLooper()).idle()
        assertNotNull(backend.startedIntent)
    }
}
