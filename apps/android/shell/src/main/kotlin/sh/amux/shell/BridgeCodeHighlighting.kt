package sh.amux.shell

import sh.amux.core.runtime.Bridge
import sh.amux.features.CodeHighlighting

/**
 * Code coloured by the shared view through the bridge: a markdown block and
 * an edit card's patch read with the same runs as on the other apps. Asked
 * off the main thread by [CodeHighlighting.follow]; a call the bridge cannot
 * answer draws the code plain.
 */
val BridgeCodeHighlighting: CodeHighlighting = CodeHighlighting { request ->
    when (request) {
        is CodeHighlighting.Request.Block -> Bridge.highlightBlock(request.code, request.info, request.streaming)
        is CodeHighlighting.Request.Patch -> Bridge.highlightPatch(request.path, request.hunks)
    }
}
