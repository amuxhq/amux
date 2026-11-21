; Runs after the grammar's own query, so these win where both match.

; A class's name is a type, however it is spelled; the grammar's query
; only guesses from capitals, and calls a one-letter name a constant.
(class_definition
  name: (identifier) @type)
