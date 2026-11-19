; What a hunk in C++ sits in: namespaces, classes, structs, unions, enums,
; aliases, functions and methods (in a class or defined outside it, as
; `Store::put`), and constants at namespace level. Lambdas and locals are
; not definitions. A template reaches up over its `template <...>` line
; through the grammar's wrapper rules. A node two patterns capture is the
; first's, so methods come before functions.

((namespace_definition name: (_) @name) @definition.module (#set! keyword "namespace"))

((class_specifier name: (_) @name body: (_)) @definition.class (#set! keyword "class"))
((struct_specifier name: (_) @name body: (_)) @definition.struct (#set! keyword "struct"))
((union_specifier name: (_) @name body: (_)) @definition.union (#set! keyword "union"))
((enum_specifier name: (_) @name body: (_)) @definition.enum (#set! keyword "enum"))
((alias_declaration name: (_) @name) @definition.type (#set! keyword "using"))
((type_definition
  type: (struct_specifier !name body: (_))
  declarator: (type_identifier) @name) @definition.struct (#set! keyword "typedef struct"))

; Methods in a class body, defined there or only declared, since a header
; holds little else. The grammar names a constructor with an identifier
; and every other method with a field identifier.
(field_declaration_list
  [
    (function_definition declarator: (function_declarator declarator: (identifier) @name))
    (declaration declarator: (function_declarator declarator: (identifier) @name))
  ] @definition.constructor
  (#set! suffix "()"))
(field_declaration_list
  [
    (function_definition declarator: (function_declarator declarator: (_) @name))
    (function_definition
      declarator: [
        (pointer_declarator (function_declarator declarator: (_) @name))
        (reference_declarator (function_declarator declarator: (_) @name))
      ])
    (field_declaration
      declarator: (function_declarator
        declarator: [(field_identifier) (destructor_name) (operator_name)] @name))
    (declaration
      declarator: (function_declarator
        declarator: [(destructor_name) (operator_name)] @name))
  ] @definition.method
  (#set! suffix "()"))
(field_declaration_list
  (template_declaration
    (function_definition declarator: (function_declarator declarator: (_) @name)) @definition.method)
  (#set! suffix "()"))

; A constructor defined outside its class (`Store::Store(int)`, or
; `Box<T>::Box()` under a template) is the function named as its class is.
; A destructor or any other method defined there is a function, as
; `Store::put` is: only a constructor has a kind of its own.
((function_definition
  declarator: (function_declarator
    declarator: [
      (qualified_identifier
        scope: [(namespace_identifier) @class (template_type name: (type_identifier) @class)]
        name: (identifier) @constructor)
      (qualified_identifier
        name: (qualified_identifier
          scope: [(namespace_identifier) @class (template_type name: (type_identifier) @class)]
          name: (identifier) @constructor))
    ] @name)) @definition.constructor
  (#eq? @class @constructor)
  (#set! suffix "()"))

((function_definition
  declarator: [
    (function_declarator declarator: (_) @name)
    (pointer_declarator declarator: (function_declarator declarator: (_) @name))
    (reference_declarator (function_declarator declarator: (_) @name))
  ]) @definition.function (#set! suffix "()"))

([(translation_unit
    (declaration
      (type_qualifier) @qualifier
      declarator: (init_declarator declarator: (identifier) @name)) @definition.constant)
  (namespace_definition
    body: (declaration_list
      (declaration
        (type_qualifier) @qualifier
        declarator: (init_declarator declarator: (identifier) @name)) @definition.constant))]
  (#any-of? @qualifier "const" "constexpr"))
