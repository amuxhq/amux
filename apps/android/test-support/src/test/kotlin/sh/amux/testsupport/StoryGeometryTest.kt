package sh.amux.testsupport

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import sh.amux.testsupport.StoryGeometry.Element
import sh.amux.testsupport.StoryGeometry.Kind
import sh.amux.testsupport.StoryGeometry.Rule

class StoryGeometryTest {
    private fun text(id: Int, left: Float, top: Float, right: Float, bottom: Float) =
        Element(id, Kind.Text, "t$id", left, top, right, bottom)

    private fun rulesOf(elements: List<Element>): List<Pair<Rule, List<Int>>> =
        StoryGeometry.flags(elements).map { it.rule to it.elements }

    @Test
    fun aRowWhoseCentresDifferByMoreThanOneDpIsFlagged() {
        // A title and its age on one line: 2 dp apart is flagged, half a dp is not.
        val off = listOf(text(1, 16f, 100f, 200f, 120f), text(2, 360f, 102f, 396f, 122f))
        val near = listOf(text(1, 16f, 100f, 200f, 120f), text(2, 360f, 100.5f, 396f, 120.5f))

        assertEquals(listOf(Rule.RowCentre to listOf(1, 2)), rulesOf(off).filter { it.first == Rule.RowCentre })
        assertTrue(rulesOf(near).none { it.first == Rule.RowCentre })
    }

    @Test
    fun textsSharingAFirstBaselineAreAlignedWhateverTheirCentres() {
        // A name and its smaller facts on one baseline: centres 1.5 dp apart,
        // baselines together. Off the baseline by 2 dp, the pair is flagged.
        val name = Element(1, Kind.Text, "name", 36f, 100f, 200f, 122f, firstBaseline = 117f)
        val facts = Element(2, Kind.Text, "desk · 1h", 300f, 103.6f, 396f, 121.4f, firstBaseline = 117f)
        val offBaseline = facts.copy(firstBaseline = 119f)

        assertTrue(rulesOf(listOf(name, facts)).none { it.first == Rule.RowCentre })
        assertEquals(
            listOf(Rule.RowCentre to listOf(1, 2)),
            rulesOf(listOf(name, offBaseline)).filter { it.first == Rule.RowCentre },
        )
    }

    @Test
    fun anEdgeJustOffASharedColumnIsFlaggedAndOneFarOffIsNot() {
        // Three rows start at 16 dp; one starts 4 dp in, one a whole indent in.
        val column = listOf(
            text(1, 16f, 0f, 300f, 20f),
            text(2, 16f, 40f, 300f, 60f),
            text(3, 16f, 80f, 300f, 100f),
        )
        val nearMiss = rulesOf(column + text(4, 20f, 120f, 300f, 140f))
        val indent = rulesOf(column + text(4, 40f, 120f, 300f, 140f))

        assertEquals(listOf(Rule.NearColumn to listOf(4)), nearMiss.filter { it.first == Rule.NearColumn })
        assertTrue(indent.none { it.first == Rule.NearColumn })
    }

    @Test
    fun aOneLabelControlDrawnTallerThanFortyFourDpIsFlaggedByItsShapeNotItsBounds() {
        // Both lay out 48 dp tall; only the one that draws 48 dp of shape is flagged.
        val pill = Element(1, Kind.Control, "Pair", 16f, 0f, 396f, 48f, leaf = false, drawnTop = 0f, drawnBottom = 48f, texts = 1)
        val button = Element(2, Kind.Control, "Pair", 16f, 100f, 396f, 148f, leaf = false, drawnTop = 104f, drawnBottom = 144f, texts = 1)
        val row = Element(3, Kind.Control, "nightly-triage", 0f, 200f, 412f, 272f, leaf = false, drawnTop = 200f, drawnBottom = 272f, texts = 3)

        assertEquals(listOf(Rule.TallControl to listOf(1)), rulesOf(listOf(pill, button, row)))
    }

    @Test
    fun aLeadingGlyphInsideTheColumnItsNeighboursShareIsFlagged() {
        // Rows of text start at 16 dp; a 48 dp button centred on the same edge
        // puts its 24 dp glyph 12 dp inside it.
        val column = listOf(
            text(1, 16f, 0f, 300f, 20f),
            text(2, 16f, 40f, 300f, 60f),
        )
        val inset = Element(3, Kind.Icon, "Back", 28f, 100f, 52f, 124f)
        val onEdge = Element(3, Kind.Icon, "Back", 16f, 100f, 40f, 124f)

        assertEquals(listOf(Rule.IconInset to listOf(3)), rulesOf(column + inset))
        assertTrue(rulesOf(column + onEdge).isEmpty())
    }
}
