; What a hunk in Swift sits in: types, extensions, protocols, functions,
; initialisers and top-level constants. Closures and properties inside a
; type are not definitions. One node kind holds classes, structs, enums,
; actors and extensions, told apart by their keyword. A node two patterns
; capture is the first's, so methods come before functions.

((class_declaration declaration_kind: "class" name: (type_identifier) @name) @definition.class (#set! keyword "class"))
((class_declaration declaration_kind: "actor" name: (type_identifier) @name) @definition.class (#set! keyword "actor"))
((class_declaration declaration_kind: "struct" name: (type_identifier) @name) @definition.struct (#set! keyword "struct"))
((class_declaration declaration_kind: "enum" name: (type_identifier) @name) @definition.enum (#set! keyword "enum"))

; `extension Store: Codable` names what it conforms to: a file often holds
; one extension of a type for each conformance.
((class_declaration
  declaration_kind: "extension"
  name: (_) @name
  (inheritance_specifier)* @conformance) @definition.impl (#set! keyword "extension"))

((protocol_declaration name: (type_identifier) @name) @definition.interface (#set! keyword "protocol"))
((typealias_declaration name: (type_identifier) @name) @definition.type (#set! keyword "typealias"))

; A function's name is found from its node's `name` field, the first of
; several the grammar gives it: any pattern reaching inside a
; `function_declaration` takes the query some 80 ms more to compile.
([(class_body (function_declaration) @definition.method)
  (enum_class_body (function_declaration) @definition.method)
  (protocol_body (protocol_function_declaration) @definition.method)]
  (#set! keyword "func"))
((function_declaration) @definition.function (#set! keyword "func"))

(init_declaration "init" @name) @definition.constructor
(deinit_declaration "deinit" @name) @definition.method

(source_file
  (property_declaration
    (value_binding_pattern mutability: "let")
    name: (pattern (simple_identifier) @name)) @definition.constant
  (#set! keyword "let"))
