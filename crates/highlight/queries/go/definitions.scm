; What a hunk in Go sits in: types, functions, methods, and package-level
; constants and variables. A function literal is not a definition. Every
; type's label begins `type`, as its declaration does; the kind says what
; the type is. A node two patterns capture is the first's, so structs and
; interfaces come before other types.

((type_spec name: (type_identifier) @name type: (struct_type)) @definition.struct (#set! keyword "type"))
((type_spec name: (type_identifier) @name type: (interface_type)) @definition.interface (#set! keyword "type"))
([
  (type_spec name: (type_identifier) @name)
  (type_alias name: (type_identifier) @name)
] @definition.type (#set! keyword "type"))

((function_declaration name: (identifier) @name) @definition.function (#set! keyword "func"))

; A method is known by its receiver's type, `(*Store).Open`, as Go's tools
; write it: methods sit at the top level, so nothing else would say whose
; it is.
((method_declaration
  receiver: (parameter_list (parameter_declaration type: (_) @receiver))
  name: (field_identifier) @name) @definition.method (#set! keyword "func"))

(source_file (const_declaration (const_spec name: (identifier) @name) @definition.constant)
  (#set! keyword "const"))
(source_file
  (var_declaration
    [
      (var_spec name: (identifier) @name) @definition.static
      (var_spec_list (var_spec name: (identifier) @name) @definition.static)
    ])
  (#set! keyword "var"))
