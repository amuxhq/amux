; What a hunk in TypeScript (and TSX) sits in: namespaces, classes,
; interfaces, type aliases, enums, functions, methods, and constants at
; module or namespace level, including those whose value is a function.
; Functions inside a body are definitions only when declared with
; `function`; an arrow function in a `const` there, a callback or an
; object's method is not. A node two patterns capture is the first's.

((internal_module name: (_) @name) @definition.module (#set! keyword "namespace"))
((module name: (_) @name) @definition.module (#set! keyword "module"))

((class_declaration name: (type_identifier) @name) @definition.class (#set! keyword "class"))
((abstract_class_declaration name: (type_identifier) @name) @definition.class (#set! keyword "abstract class"))
((interface_declaration name: (type_identifier) @name) @definition.interface (#set! keyword "interface"))
((type_alias_declaration name: (type_identifier) @name) @definition.type (#set! keyword "type"))
((enum_declaration name: (identifier) @name) @definition.enum (#set! keyword "enum"))

(class_body
  (method_definition "get" name: (_) @name) @definition.method
  (#set! keyword "get"))
(class_body
  (method_definition "set" name: (_) @name) @definition.method
  (#set! keyword "set"))
(class_body
  (method_definition name: (property_identifier) @name) @definition.constructor
  (#eq? @name "constructor"))
([(class_body
    [
      (method_definition name: (_) @name)
      (abstract_method_signature name: (_) @name)
      (method_signature name: (_) @name)
      (public_field_definition
        name: (_) @name
        value: [(arrow_function) (function_expression)])
    ] @definition.method)
  (interface_body (method_signature name: (_) @name) @definition.method)]
  (#set! suffix "()"))

((function_declaration name: (identifier) @name) @definition.function (#set! keyword "function"))
((generator_function_declaration name: (identifier) @name) @definition.function (#set! keyword "function*"))
((function_signature name: (identifier) @name) @definition.function (#set! keyword "function"))

; Module level is the program and a namespace's body; `export` appears
; only there.
([(program
    (lexical_declaration "const"
      (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function))
  (internal_module
    body: (statement_block
      (lexical_declaration "const"
        (variable_declarator
          name: (identifier) @name
          value: [(arrow_function) (function_expression)]) @definition.function)))
  (export_statement
    declaration: (lexical_declaration "const"
      (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function))]
  (#set! keyword "const"))
([(program
    (lexical_declaration "const"
      (variable_declarator name: (identifier) @name) @definition.constant))
  (internal_module
    body: (statement_block
      (lexical_declaration "const"
        (variable_declarator name: (identifier) @name) @definition.constant)))
  (export_statement
    declaration: (lexical_declaration "const"
      (variable_declarator name: (identifier) @name) @definition.constant))]
  (#set! keyword "const"))
