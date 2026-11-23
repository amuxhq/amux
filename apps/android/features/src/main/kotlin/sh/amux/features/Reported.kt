package sh.amux.features

import androidx.compose.ui.semantics.SemanticsPropertyKey
import androidx.compose.ui.semantics.SemanticsPropertyReceiver

/*
 * What an element reads as to the debug app's driving door, where that is
 * not what TalkBack should say.
 *
 * The door reports each tagged element's label and value from the semantics
 * tree, and the phone journeys read the same label and value on every
 * client. Most of the time the spoken words are the right reading: a
 * `stateDescription` such as "queued" is what a person hears and what a
 * journey checks. Some readings would make TalkBack say a thing twice or say
 * a word meant for a machine: a page's account email, a title's own name as
 * its value, a header's "live". Those are declared here instead, on keys
 * accessibility services never read, so the spoken interface stays as it is
 * and the door still reads what the journeys check.
 *
 * Neither key merges into an ancestor: each reading belongs to the element
 * that declares it, and a row must not take on the value of a control drawn
 * inside it.
 */

/** The label the door reports in place of the element's spoken words. */
val ReportedLabel: SemanticsPropertyKey<String> =
    SemanticsPropertyKey(name = "ReportedLabel", mergePolicy = { parent, _ -> parent })

/** The value the door reports in place of the element's text or state. */
val ReportedValue: SemanticsPropertyKey<String> =
    SemanticsPropertyKey(name = "ReportedValue", mergePolicy = { parent, _ -> parent })

var SemanticsPropertyReceiver.reportedLabel: String by ReportedLabel
var SemanticsPropertyReceiver.reportedValue: String by ReportedValue
