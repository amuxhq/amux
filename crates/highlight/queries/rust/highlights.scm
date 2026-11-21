; Runs after the grammar's own query, so these win where both match.

; All-caps names are constants. The grammar's own pattern for this has a
; stray quote at the end of its regex and never matches.
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z\\d_]+$"))
