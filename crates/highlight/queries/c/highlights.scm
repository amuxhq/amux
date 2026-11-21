; Runs after the grammar's own query (and C++'s, which extends it), so
; these win where both match.

; Neither grammar's query colours the boolean literals.
[
  (true)
  (false)
] @boolean
