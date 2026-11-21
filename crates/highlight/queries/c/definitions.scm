; What a hunk in C sits in: functions, the structs, unions and enums given
; a body, and file-level constants. Declarations without a body and macros
; are not definitions. A C function has no keyword, so its label is its
; name and parentheses.

((function_definition
  declarator: [
    (function_declarator declarator: (_) @name)
    (pointer_declarator declarator: (function_declarator declarator: (_) @name))
    (pointer_declarator
      declarator: (pointer_declarator declarator: (function_declarator declarator: (_) @name)))
  ]) @definition.function (#set! suffix "()"))

((struct_specifier name: (_) @name body: (_)) @definition.struct (#set! keyword "struct"))
((union_specifier name: (_) @name body: (_)) @definition.union (#set! keyword "union"))
((enum_specifier name: (_) @name body: (_)) @definition.enum (#set! keyword "enum"))

; `typedef struct { ... } store;` is the common way to name a struct, and
; the typedef's name is its only one.
((type_definition
  type: (struct_specifier !name body: (_))
  declarator: (type_identifier) @name) @definition.struct (#set! keyword "typedef struct"))
((type_definition
  type: (union_specifier !name body: (_))
  declarator: (type_identifier) @name) @definition.union (#set! keyword "typedef union"))
((type_definition
  type: (enum_specifier !name body: (_))
  declarator: (type_identifier) @name) @definition.enum (#set! keyword "typedef enum"))

(translation_unit
  (declaration
    (type_qualifier) @qualifier
    declarator: (init_declarator declarator: (identifier) @name)) @definition.constant
  (#eq? @qualifier "const"))
