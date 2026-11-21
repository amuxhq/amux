; What a hunk in Rust sits in: modules, types, impls, functions, macros and
; item-level constants. Closures and `let`s are not definitions; a `const`
; or `static` counts only outside function bodies, which are `block`s.
; A node two patterns capture is the first's, so methods come before
; functions.

((mod_item name: (identifier) @name) @definition.module (#set! keyword "mod"))

((struct_item name: (type_identifier) @name) @definition.struct (#set! keyword "struct"))
((enum_item name: (type_identifier) @name) @definition.enum (#set! keyword "enum"))
((union_item name: (type_identifier) @name) @definition.union (#set! keyword "union"))
((trait_item name: (type_identifier) @name) @definition.interface (#set! keyword "trait"))
((type_item name: (type_identifier) @name) @definition.type (#set! keyword "type"))

; `impl Display for Store` names the trait too: a file often holds several
; impls of one type, and the type alone would not say which.
((impl_item trait: (_) @trait type: (_) @name) @definition.impl (#set! keyword "impl"))
((impl_item !trait type: (_) @name) @definition.impl (#set! keyword "impl"))

(impl_item
  body: (declaration_list
    (function_item name: (identifier) @name) @definition.method)
  (#set! keyword "fn"))
(trait_item
  body: (declaration_list
    [
      (function_item name: (identifier) @name)
      (function_signature_item name: (identifier) @name)
    ] @definition.method)
  (#set! keyword "fn"))
((function_item name: (identifier) @name) @definition.function (#set! keyword "fn"))

((macro_definition name: (identifier) @name) @definition.macro (#set! keyword "macro_rules!"))

([(source_file (const_item name: (identifier) @name) @definition.constant)
  (declaration_list (const_item name: (identifier) @name) @definition.constant)]
  (#set! keyword "const"))
([(source_file (static_item name: (identifier) @name) @definition.static)
  (declaration_list (static_item name: (identifier) @name) @definition.static)]
  (#set! keyword "static"))
