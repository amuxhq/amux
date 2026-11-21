package sh.amux.shell

import java.io.ByteArrayInputStream
import java.io.InputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class ShellPlatformTest {
    private val pngHeader = byteArrayOf(
        0x89.toByte(),
        0x50,
        0x4E,
        0x47,
        0x0D,
        0x0A,
        0x1A,
        0x0A,
        0x00,
        0x00,
        0x00,
        0x0D,
    )
    private val jpegHeader = byteArrayOf(
        0xFF.toByte(),
        0xD8.toByte(),
        0xFF.toByte(),
        0xE0.toByte(),
        0x00,
        0x10,
    )
    private val gifHeader = "GIF89a\u0001\u0000\u0001\u0000".encodeToByteArray()
    private val webpHeader = "RIFF\u0010\u0000\u0000\u0000WEBPVP8 ".encodeToByteArray()
    private val heicHeader = byteArrayOf(
        0x00,
        0x00,
        0x00,
        0x18,
        'f'.code.toByte(),
        't'.code.toByte(),
        'y'.code.toByte(),
        'p'.code.toByte(),
        'h'.code.toByte(),
        'e'.code.toByte(),
        'i'.code.toByte(),
        'c'.code.toByte(),
    )

    @Test
    fun readStreamBoundedReturnsBytesUnderAndAtExactLimit() {
        val under = "hello".encodeToByteArray()
        val underResult = ShellPlatform.readStreamBounded(ByteArrayInputStream(under), maxBytes = 10L)
        assertFalse(underResult.exceededLimit)
        assertArrayEquals(under, underResult.bytes)

        val exact = ByteArray(10) { it.toByte() }
        val exactResult = ShellPlatform.readStreamBounded(ByteArrayInputStream(exact), maxBytes = 10L)
        assertFalse(exactResult.exceededLimit)
        assertArrayEquals(exact, exactResult.bytes)
    }

    @Test
    fun readStreamBoundedStopsAtLimitPlusOneWithoutBufferingUnboundedStream() {
        var bytesPulled = 0L
        val endless = object : InputStream() {
            override fun read(): Int {
                bytesPulled += 1L
                return 0x41
            }

            override fun read(b: ByteArray, off: Int, len: Int): Int {
                val n = len.coerceAtMost(4096)
                b.fill(0x41, off, off + n)
                bytesPulled += n
                return n
            }
        }

        val limit = 1024L
        val result = ShellPlatform.readStreamBounded(endless, maxBytes = limit)

        assertTrue(result.exceededLimit)
        assertEquals(0, result.bytes.size)
        assertEquals(limit + 1L, bytesPulled)
    }

    @Test
    fun classifyPickedBytesRecognizesNativeImageMagicHeaders() {
        val png = ShellPlatform.classifyPickedBytes(pngHeader, "shot.bin", null, isPhoto = false)
        assertTrue(png.image)
        assertEquals("image/png", png.mime)

        val jpeg = ShellPlatform.classifyPickedBytes(jpegHeader, "cam.dat", "application/octet-stream", isPhoto = true)
        assertTrue(jpeg.image)
        assertEquals("image/jpeg", jpeg.mime)

        val gif = ShellPlatform.classifyPickedBytes(gifHeader, "anim.gif", null, isPhoto = false)
        assertTrue(gif.image)
        assertEquals("image/gif", gif.mime)

        val webp = ShellPlatform.classifyPickedBytes(webpHeader, "clip.webp", null, isPhoto = false)
        assertTrue(webp.image)
        assertEquals("image/webp", webp.mime)
    }

    @Test
    fun classifyPickedBytesTranscodesHeicPhotoWhenDecoderSucceeds() {
        val picked = ShellPlatform.classifyPickedBytes(
            bytes = heicHeader,
            name = "IMG_0001.HEIC",
            reportedMime = "image/heic",
            isPhoto = true,
            transcodeToNativeImage = { jpegHeader to "image/jpeg" },
        )

        assertTrue(picked.image)
        assertEquals("image/jpeg", picked.mime)
        assertEquals("IMG_0001.jpg", picked.name)
        assertArrayEquals(jpegHeader, picked.data)
    }

    @Test
    fun classifyPickedBytesFallsBackToFileWhenHeicCannotDecodeOrIsNotNativeImage() {
        val failedHeic = ShellPlatform.classifyPickedBytes(
            bytes = heicHeader,
            name = "IMG_0002.heic",
            reportedMime = "image/heic",
            isPhoto = true,
            transcodeToNativeImage = { null },
        )
        assertFalse(failedHeic.image)
        assertEquals("IMG_0002.heic", failedHeic.name)
        assertArrayEquals(heicHeader, failedHeic.data)

        val pdfBytes = "%PDF-1.7\nhello".encodeToByteArray()
        val pdf = ShellPlatform.classifyPickedBytes(
            bytes = pdfBytes,
            name = "spec.pdf",
            reportedMime = "application/pdf",
            isPhoto = false,
        )
        assertFalse(pdf.image)
        assertEquals("application/pdf", pdf.mime)

        val fakePng = ShellPlatform.classifyPickedBytes(
            bytes = "plain text masquerading as png".encodeToByteArray(),
            name = "notes.png",
            reportedMime = "image/png",
            isPhoto = false,
            transcodeToNativeImage = { null },
        )
        assertFalse(fakePng.image)
    }

    @Test
    fun boundedByteArrayOutputStreamEnforcesLimitOnSingleAndBulkWrites() {
        val exact = ShellPlatform.BoundedByteArrayOutputStream(maxBytes = 5L)
        exact.write("hel".encodeToByteArray())
        exact.write('l'.code)
        exact.write('o'.code)
        assertArrayEquals("hello".encodeToByteArray(), exact.toByteArray())

        try {
            exact.write('!'.code)
            fail("expected OutputLimitExceededException on single-byte overflow")
        } catch (_: ShellPlatform.OutputLimitExceededException) {
            // expected
        }

        val bulk = ShellPlatform.BoundedByteArrayOutputStream(maxBytes = 4L)
        bulk.write("ab".encodeToByteArray())
        try {
            bulk.write("cde".encodeToByteArray(), 0, 3)
            fail("expected OutputLimitExceededException on bulk overflow")
        } catch (_: ShellPlatform.OutputLimitExceededException) {
            // expected
        }
        assertArrayEquals("ab".encodeToByteArray(), bulk.toByteArray())
    }

    @Test
    fun boundedTranscodeSizePreservesAspectRatioWithinDimensionAndPixelCaps() {
        assertEquals(1024 to 768, ShellPlatform.boundedTranscodeSize(1024, 768))
        assertEquals(4096 to 2048, ShellPlatform.boundedTranscodeSize(8192, 4096))
        assertEquals(2048 to 4096, ShellPlatform.boundedTranscodeSize(4096, 8192))

        val pixelCapped = ShellPlatform.boundedTranscodeSize(
            width = 4000,
            height = 3000,
            maxDimension = 4096,
            maxPixels = 4_000_000L,
        )!!
        assertTrue(pixelCapped.first <= 4096 && pixelCapped.second <= 4096)
        assertTrue(pixelCapped.first.toLong() * pixelCapped.second.toLong() <= 4_000_000L)
        val originalRatio = 4000.0 / 3000.0
        val scaledRatio = pixelCapped.first.toDouble() / pixelCapped.second.toDouble()
        assertTrue(kotlin.math.abs(originalRatio - scaledRatio) < 0.01)

        assertNull(ShellPlatform.boundedTranscodeSize(0, 100))
        assertNull(ShellPlatform.boundedTranscodeSize(100, -1))
    }
}
