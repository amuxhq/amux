; What a hunk in Python sits in: classes, functions and methods (nested
; ones too, since a nested def is named and often long), and the all-caps
; names a module or class uses as constants. Lambdas and other assignments
; are not definitions. A node two patterns capture is the first's, so
; methods come before functions. A decorated definition reaches up over
; its decorators through the grammar's wrapper rules.

((class_definition name: (identifier) @name) @definition.class (#set! keyword "class"))

(class_definition
  body: (block
    [
      (function_definition name: (identifier) @name) @definition.method
      (decorated_definition
        definition: (function_definition name: (identifier) @name) @definition.method)
    ])
  (#set! keyword "def"))
((function_definition name: (identifier) @name) @definition.function (#set! keyword "def"))

([(module (expression_statement (assignment left: (identifier) @name)) @definition.constant)
  (class_definition
    body: (block (expression_statement (assignment left: (identifier) @name)) @definition.constant))]
  (#match? @name "^[A-Z][A-Z0-9_]*$"))
