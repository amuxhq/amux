package sh.amux.words

import sh.amux.values.ProsePart
import sh.amux.values.Segment

/** The part as the segment it was parsed from: its words as written. */
val ProsePart.segment: Segment
    get() = when (this) {
        is ProsePart.Text -> Segment.Text(value.source)
        is ProsePart.Attachment -> Segment.Attachment(value)
    }
