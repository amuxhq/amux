package sh.amux.features

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

// Robolectric's Android, because DecodedImages holds an android.util.LruCache.
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class DecodedImagesTest {
    @Test
    fun aFittedImageKeepsItsLongerSideCovered() {
        assertEquals(1, DecodedImages.sampleSize(width = 400, height = 300, maxPx = 300))
        assertEquals(4, DecodedImages.sampleSize(width = 4000, height = 3000, maxPx = 960))
    }

    @Test
    fun aCroppedPanoramaKeepsItsShorterSideCovered() {
        // A 12000 × 1000 panorama in a 72 px square chip: fitted, its short
        // side would shrink to about 8 px and blur; cropped, it stays above 72.
        assertEquals(128, DecodedImages.sampleSize(width = 12_000, height = 1_000, maxPx = 72))
        assertEquals(8, DecodedImages.sampleSize(width = 12_000, height = 1_000, maxPx = 72, crop = true))
    }
}
