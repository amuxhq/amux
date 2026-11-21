; Runs after the grammar's own query, so these win where both match.

; The grammar's query marks every field identifier a property after its
; method patterns, which hides method names; they are restated here.
(call_expression
  function: (selector_expression
    field: (field_identifier) @function.method))

(method_declaration
  name: (field_identifier) @function.method)

(method_elem
  name: (field_identifier) @function.method)
