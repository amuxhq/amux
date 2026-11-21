package sh.amux.shell

import java.io.ByteArrayInputStream
import java.io.InputStream
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
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
}
