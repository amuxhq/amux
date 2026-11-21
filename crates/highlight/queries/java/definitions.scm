; What a hunk in Java sits in: classes, interfaces, enums, records,
; methods, constructors and `static final` constants. Lambdas, locals and
; the members of anonymous classes are not definitions, so members count
; only in the body of a declared type. Java has no keyword for a method,
; so its label is its name and parentheses.

((class_declaration name: (identifier) @name) @definition.class (#set! keyword "class"))
((record_declaration name: (identifier) @name) @definition.class (#set! keyword "record"))
((interface_declaration name: (identifier) @name) @definition.interface (#set! keyword "interface"))
((annotation_type_declaration name: (identifier) @name) @definition.interface (#set! keyword "@interface"))
((enum_declaration name: (identifier) @name) @definition.enum (#set! keyword "enum"))

([(class_declaration body: (class_body (method_declaration name: (identifier) @name) @definition.method))
  (record_declaration body: (class_body (method_declaration name: (identifier) @name) @definition.method))
  (interface_declaration body: (interface_body (method_declaration name: (identifier) @name) @definition.method))
  (enum_declaration
    body: (enum_body (enum_body_declarations (method_declaration name: (identifier) @name) @definition.method)))]
  (#set! suffix "()"))

([(class_declaration body: (class_body (constructor_declaration name: (identifier) @name) @definition.constructor))
  (record_declaration body: (class_body (constructor_declaration name: (identifier) @name) @definition.constructor))
  (enum_declaration
    body: (enum_body (enum_body_declarations (constructor_declaration name: (identifier) @name) @definition.constructor)))]
  (#set! suffix "()"))
(record_declaration
  body: (class_body (compact_constructor_declaration name: (identifier) @name) @definition.constructor))

([(class_declaration body: (class_body (field_declaration
    (modifiers) @modifiers
    declarator: (variable_declarator name: (identifier) @name)) @definition.constant))
  (interface_declaration body: (interface_body (constant_declaration
    declarator: (variable_declarator name: (identifier) @name)) @definition.constant))]
  (#match? @modifiers "\\bstatic\\b")
  (#match? @modifiers "\\bfinal\\b"))
