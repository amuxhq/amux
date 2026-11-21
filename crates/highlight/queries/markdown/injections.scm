; A paragraph's or heading's text goes to the inline grammar, for code
; spans, emphasis and links. Its children are included: the block grammar
; leaves the inline markers (backticks, brackets) as children of the text,
; and without them the inline grammar cannot find a code span.
;
; The grammar's own injections also send fenced code, HTML blocks and front
; matter to their languages; those are left out, so a Markdown file never
; waits for every grammar it might quote.
((inline) @injection.content
 (#set! injection.language "markdown_inline")
 (#set! injection.include-children))
