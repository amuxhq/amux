; What a hunk in JavaScript sits in: classes, functions, methods, and
; constants at module level, including those whose value is a function.
; Functions and arrow functions inside a body are definitions only when
; declared with `function`; an arrow function in a `const` there, a
; callback or an object's method is not. A node two patterns capture is
; the first's.

((class_declaration name: (identifier) @name) @definition.class (#set! keyword "class"))

; Members count only in a declared class's body: the methods of a class
; expression (`() => class { run() {} }`) would otherwise sit under the
; constant or function the expression is in, which holds no class.
(class_declaration
  body: (class_body (method_definition "get" name: (_) @name) @definition.method)
  (#set! keyword "get"))
(class_declaration
  body: (class_body (method_definition "set" name: (_) @name) @definition.method)
  (#set! keyword "set"))
(class_declaration
  body: (class_body (method_definition name: (property_identifier) @name) @definition.constructor)
  (#eq? @name "constructor"))
(class_declaration
  body: (class_body
    [
      (method_definition name: (_) @name)
      (field_definition
        property: (_) @name
        value: [(arrow_function) (function_expression)])
    ] @definition.method)
  (#set! suffix "()"))

((function_declaration name: (identifier) @name) @definition.function (#set! keyword "function"))
((generator_function_declaration name: (identifier) @name) @definition.function (#set! keyword "function*"))

([(program
    (lexical_declaration "const"
      (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function))
  (export_statement
    declaration: (lexical_declaration "const"
      (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function))]
  (#set! keyword "const"))
([(program
    (lexical_declaration "const"
      (variable_declarator name: (identifier) @name) @definition.constant))
  (export_statement
    declaration: (lexical_declaration "const"
      (variable_declarator name: (identifier) @name) @definition.constant))]
  (#set! keyword "const"))
