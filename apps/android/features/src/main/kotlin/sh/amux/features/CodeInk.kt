package sh.amux.features

import androidx.compose.ui.graphics.Color
import sh.amux.design.AmuxColors
import sh.amux.values.TokenKind

/**
 * The colour a kind of code token is drawn in: names and punctuation in the
 * reading ink, and a block written as a diff in a change's green and red.
 *
 * Here rather than in the design module, which holds the colours and knows
 * nothing of the bridge's values: what a token is comes from the shared
 * view, and the screens that draw code are what pair the two.
 */
internal fun AmuxColors.ink(kind: TokenKind): Color = when (kind) {
    TokenKind.Plain -> ink
    TokenKind.Keyword -> syntax.keyword
    TokenKind.String -> syntax.string
    TokenKind.Number -> syntax.number
    TokenKind.Type -> syntax.type
    TokenKind.Function -> syntax.function
    TokenKind.Comment -> syntax.comment
    TokenKind.Inserted -> added
    TokenKind.Deleted -> removed
}
