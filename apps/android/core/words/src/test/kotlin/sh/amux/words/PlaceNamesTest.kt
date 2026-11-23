package sh.amux.words

import org.junit.Assert.assertEquals
import org.junit.Test

class PlaceNamesTest {
    @Test
    fun theLocalSuffixIsDropped() {
        assertEquals("Adas-MacBook-Pro", PlaceNames.host("Adas-MacBook-Pro.local"))
        assertEquals("studio", PlaceNames.host("studio.LOCAL"))
        assertEquals("studio", PlaceNames.host("studio"))
        assertEquals(".local", PlaceNames.host(".local"))
    }

    @Test
    fun directoriesAreWrittenTheWayAShellPromptWritesThem() {
        assertEquals("~/s/amux", PlaceNames.directory("/Users/ada/source/amux"))
        assertEquals("~/w/a/server", PlaceNames.directory("/home/ada/work/api/server"))
        assertEquals("~/s/amux", PlaceNames.directory("~/src/amux"))
        assertEquals("~", PlaceNames.directory("/Users/ada"))
        assertEquals("~/work", PlaceNames.directory("/root/work"))
        assertEquals("/U/S/tally", PlaceNames.directory("/Users/Shared/tally"))
        assertEquals("~/.c/amux", PlaceNames.directory("/Users/ada/.config/amux"))
        assertEquals("/o/b/amux", PlaceNames.directory("/opt/build/amux"))
        assertEquals("amux", PlaceNames.directory("amux"))
    }

    @Test
    fun aPathReadWholeWritesOnlyItsHomeAsATilde() {
        assertEquals("~/source/amux", PlaceNames.path("/Users/ada/source/amux"))
        assertEquals("~/work/api", PlaceNames.path("/home/ada/work/api"))
        assertEquals("~", PlaceNames.path("/Users/ada"))
        assertEquals("/Users/Shared/tally", PlaceNames.path("/Users/Shared/tally"))
        assertEquals("/opt/build/amux", PlaceNames.path("/opt/build/amux"))
    }

    @Test
    fun thePlaceLeadsWithTheDirectory() {
        assertEquals("~/s/amux · studio", PlaceNames.place(host = "studio.local", directory = "/Users/ada/source/amux"))
        assertEquals("~/s/amux", PlaceNames.place(host = null, directory = "/Users/ada/source/amux"))
        assertEquals("studio", PlaceNames.place(host = "studio", directory = ""))
    }
}
