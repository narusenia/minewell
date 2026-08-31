; SPDX-License-Identifier: MIT
; Highlighting for .mwl. nvim and zed both read this file.

; ---- comments and literals ----

(line_comment) @comment
(block_comment) @comment

(integer_literal) @number
(boolean_literal) @boolean
(string_literal) @string

; The two literals that only exist because Minecraft does. Giving them their own
; colour is most of the point of highlighting this language.
(selector) @constant.builtin
(resource_location) @string.special

; ---- keywords ----

[
  "fn" "let" "mut" "const" "struct" "enum" "impl" "in"
] @keyword

[ "if" "else" "match" "=>" ] @keyword.conditional
[ "while" "loop" "for" "break" "continue" ] @keyword.repeat
"return" @keyword.return

; Execution context, which is the language's own idea (spec section 6.15).
[ "as" "at" ] @keyword.modifier

[ "Some" "None" ] @constant.builtin

; ---- operators and punctuation ----

[
  "+" "-" "*" "/" "%"
  "==" "!=" "<" "<=" ">" ">="
  "&&" "||" "!"
  "=" "+=" "-=" "*=" "/=" "%="
  "&" "?" ".." "..="
] @operator

[ "(" ")" "[" "]" "{" "}" ] @punctuation.bracket
[ "," ";" ":" "::" "->" "." ] @punctuation.delimiter

; ---- names ----

(attribute) @attribute

(function_item name: (identifier) @function)
(call_expression function: (identifier) @function.call)
(method_call name: (identifier) @function.method.call)
(turbofish_call type: (identifier) @type)

(macro_invocation name: (identifier) @function.macro)
"!" @function.macro

(type_identifier) @type
"fix" @type.builtin

(parameter name: (identifier) @variable.parameter)
(field_declaration name: (identifier) @variable.member)
(field_initializer name: (identifier) @variable.member)
(field_expression field: (identifier) @variable.member)

(path_expression type: (identifier) @type)
(struct_expression name: (identifier) @type)
(variant_pattern (identifier) @type)

(identifier) @variable
