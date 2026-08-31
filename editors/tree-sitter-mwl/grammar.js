// SPDX-License-Identifier: MIT

// The .mwl grammar, from docs/02-spec.md sections 2 and 3.
//
// This is for highlighting, not for compiling: it has to accept everything the
// compiler accepts, and it does not have to reject what the compiler rejects. Where
// the two could differ, this one is the permissive side.
//
// Three things here are not obvious, and all three come from the spec:
//
//   - a resource location (`minecraft:block.note_block.pling`) is ONE token, dots and
//     hyphens included, and is told apart from a type annotation only by there being
//     no space around the `:` (spec section 2.8)
//   - a selector (`@e[type=zombie, distance=..8]`) is one token whose brackets are
//     balanced, and a `]` inside a string does not close it (spec section 2.7)
//   - `fix::<1000>` puts `::<` where only a type argument can go (spec section 3.16)

const PREC = {
  or: 1,
  and: 2,
  compare: 3,
  add: 4,
  mul: 5,
  unary: 6,
  call: 7,
  field: 8,
};

module.exports = grammar({
  name: 'mwl',

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

  word: ($) => $.identifier,

  // `if x { .. }` and `Point { .. }` both start `identifier {`. The real parser keeps
  // a flag for this (spec section 3); here the two readings are explored side by side
  // and the block wins, because a struct literal in that position is the rarer thing
  // and can always be parenthesised.
  // `if x { .. }` and `Point { .. }` both start `identifier {`. The real parser keeps
  // a flag for this (spec section 3); here the two readings are explored side by side
  // and the block wins, because a struct literal in that position is the rarer thing
  // and can always be parenthesised.
  conflicts: ($) => [
    [$._expression, $.struct_expression],
    [$.path_expression, $.struct_expression],
  ],

  rules: {
    source_file: ($) => repeat($._item),

    // ---- comments ----

    line_comment: ($) => token(seq('//', /.*/)),

    // Block comments nest (spec section 2.2), which is why this is not a regex.
    block_comment: ($) =>
      seq('/*', repeat(choice($.block_comment, /[^*/]+/, '*', '/')), '*/'),

    // ---- items ----

    _item: ($) =>
      choice(
        $.function_item,
        $.struct_item,
        $.enum_item,
        $.impl_item,
      ),

    attribute: ($) => seq('#', '[', repeat($._token_tree), ']'),

    function_item: ($) =>
      seq(
        repeat($.attribute),
        'fn',
        field('name', $.identifier),
        optional($.generic_parameters),
        $.parameters,
        optional(seq('->', field('return_type', $._type))),
        field('body', $.block),
      ),

    generic_parameters: ($) =>
      seq(
        '<',
        commaSep1(choice(seq('const', $.identifier, ':', $._type), $.identifier)),
        '>',
      ),

    parameters: ($) => seq('(', commaSep(choice($.self_parameter, $.parameter)), ')'),

    self_parameter: ($) => seq(optional(seq('&', optional('mut'))), 'self'),

    parameter: ($) => seq(field('name', $.identifier), ':', field('type', $._type)),

    struct_item: ($) =>
      seq(
        repeat($.attribute),
        'struct',
        field('name', $._type_identifier),
        optional($.generic_parameters),
        $.field_declaration_list,
      ),

    field_declaration_list: ($) => seq('{', commaSep($.field_declaration), '}'),

    field_declaration: ($) =>
      seq(
        repeat($.attribute),
        field('name', $.identifier),
        ':',
        field('type', $._type),
      ),

    enum_item: ($) =>
      seq(
        repeat($.attribute),
        'enum',
        field('name', $._type_identifier),
        '{',
        commaSep($.variant),
        '}',
      ),

    variant: ($) =>
      seq(field('name', $._type_identifier), optional($.field_declaration_list)),

    impl_item: ($) =>
      seq('impl', field('type', $._type_identifier), '{', repeat($.function_item), '}'),

    // ---- types ----

    _type: ($) =>
      choice($.reference_type, $.generic_type, $.fixed_type, $._type_identifier),

    reference_type: ($) => seq('&', optional('mut'), $._type),

    generic_type: ($) =>
      seq(field('name', $._type_identifier), '<', commaSep1($._type), '>'),

    // `fix<1000>` and `fix<S>`: the one type that takes a value as an argument.
    fixed_type: ($) => seq('fix', '<', choice($.integer_literal, $.identifier), '>'),

    // Aliased rather than wrapped: a node of its own would make the parser choose
    // between "identifier" and "type identifier" before it has seen enough to know.
    _type_identifier: ($) => alias($.identifier, $.type_identifier),

    // ---- statements ----

    block: ($) => seq('{', repeat($._statement), '}'),

    _statement: ($) =>
      choice(
        $.let_statement,
        $.if_statement,
        $.while_statement,
        $.loop_statement,
        $.for_statement,
        $.context_statement,
        $.match_statement,
        $.break_statement,
        $.continue_statement,
        $.return_statement,
        $.expression_statement,
      ),

    let_statement: ($) =>
      seq(
        'let',
        optional('mut'),
        field('name', $.identifier),
        // The space after `:` is required; without it this is a resource location
        // (spec section 2.8), which is why the type is written as its own token here.
        optional(seq(':', field('type', $._type))),
        '=',
        field('value', $._expression),
        ';',
      ),

    if_statement: ($) =>
      seq(
        repeat($.attribute),
        'if',
        choice($.let_condition, field('condition', $._expression)),
        field('consequence', $.block),
        optional(seq('else', field('alternative', choice($.block, $.if_statement)))),
      ),

    let_condition: ($) => seq('let', $.pattern, '=', $._expression),

    while_statement: ($) =>
      seq(repeat($.attribute), 'while', field('condition', $._expression), $.block),

    loop_statement: ($) => seq(repeat($.attribute), 'loop', $.block),

    for_statement: ($) =>
      seq(
        repeat($.attribute),
        'for',
        field('binding', $.identifier),
        'in',
        field('source', $._expression),
        $.block,
      ),

    // `as @s { .. }` and `at @s { .. }` (spec section 6.15).
    context_statement: ($) =>
      seq(repeat($.attribute), choice('as', 'at'), field('selector', $._expression), $.block),

    match_statement: ($) =>
      seq('match', field('value', $._expression), '{', repeat($.match_arm), '}'),

    match_arm: ($) => seq($.pattern, '=>', choice($.block, seq($._expression, ';'))),

    pattern: ($) =>
      choice(
        $.wildcard_pattern,
        $.some_pattern,
        $.none_pattern,
        $.variant_pattern,
      ),

    wildcard_pattern: ($) => '_',
    some_pattern: ($) => seq('Some', '(', $.identifier, ')'),
    none_pattern: ($) => 'None',

    variant_pattern: ($) =>
      seq(
        $.identifier,
        optional(seq('::', $._type_identifier)),
        optional(seq('{', commaSep($.identifier), '}')),
      ),

    break_statement: ($) => seq('break', ';'),
    continue_statement: ($) => seq('continue', ';'),
    return_statement: ($) => seq('return', optional($._expression), ';'),

    expression_statement: ($) => seq($._expression, ';'),

    // ---- expressions ----

    _expression: ($) =>
      choice(
        $.integer_literal,
        $.boolean_literal,
        $.string_literal,
        $.selector,
        $.resource_location,
        $.macro_invocation,
        $.identifier,
        $.some_expression,
        $.none_expression,
        $.path_expression,
        $.struct_expression,
        $.list_expression,
        $.unary_expression,
        $.binary_expression,
        $.assignment_expression,
        $.call_expression,
        $.turbofish_call,
        $.method_call,
        $.field_expression,
        $.index_expression,
        $.try_expression,
        $.range_expression,
        $.parenthesized_expression,
      ),

    parenthesized_expression: ($) => seq('(', $._expression, ')'),

    some_expression: ($) => seq('Some', '(', $._expression, ')'),
    none_expression: ($) => 'None',

    // `Threat::Calm`: a unit variant, which is a value on its own.
    // The head is a plain identifier on purpose: deciding it is a type name before
    // the `::` has been seen is a choice the parser cannot make yet.
    path_expression: ($) =>
      seq(field('type', $.identifier), '::', field('name', $._type_identifier)),

    struct_expression: ($) =>
      prec.dynamic(
        -1,
        seq(
          field('name', $.identifier),
          optional(seq('::', field('variant', $._type_identifier))),
          $.field_initializer_list,
        ),
      ),

    field_initializer_list: ($) => seq('{', commaSep($.field_initializer), '}'),

    field_initializer: ($) =>
      seq(field('name', $.identifier), ':', field('value', $._expression)),

    list_expression: ($) => seq('[', commaSep($._expression), ']'),

    unary_expression: ($) =>
      prec(PREC.unary, seq(choice('-', '!', '&'), optional('mut'), $._expression)),

    binary_expression: ($) => {
      const table = [
        [PREC.or, '||'],
        [PREC.and, '&&'],
        [PREC.compare, choice('==', '!=', '<', '<=', '>', '>=')],
        [PREC.add, choice('+', '-')],
        [PREC.mul, choice('*', '/', '%')],
      ];
      return choice(
        ...table.map(([precedence, operator]) =>
          prec.left(
            precedence,
            seq(
              field('left', $._expression),
              field('operator', operator),
              field('right', $._expression),
            ),
          ),
        ),
      );
    },

    assignment_expression: ($) =>
      prec.right(
        0,
        seq(
          field('left', $._expression),
          choice('=', '+=', '-=', '*=', '/=', '%='),
          field('right', $._expression),
        ),
      ),

    call_expression: ($) =>
      prec(PREC.call, seq(field('function', $.identifier), $.arguments)),

    // `fix::<1000>(1500)` and `Mob::of(@s)`: `::` where a type argument or an
    // associated name goes.
    turbofish_call: ($) =>
      prec(
        PREC.call,
        seq(
          field('type', $.identifier),
          '::',
          choice(
            seq('<', commaSep1(choice($.integer_literal, $._type)), '>'),
            field('name', $.identifier),
          ),
          $.arguments,
        ),
      ),

    method_call: ($) =>
      prec(
        PREC.field,
        seq(field('receiver', $._expression), '.', field('name', $.identifier), $.arguments),
      ),

    field_expression: ($) =>
      prec(PREC.field, seq(field('value', $._expression), '.', field('field', $.identifier))),

    index_expression: ($) =>
      prec(PREC.field, seq($._expression, '[', $._expression, ']')),

    try_expression: ($) => prec(PREC.field, seq($._expression, '?')),

    range_expression: ($) =>
      prec.left(
        0,
        seq(optional($._expression), choice('..=', '..'), optional($._expression)),
      ),

    arguments: ($) => seq('(', commaSep($._expression), ')'),

    // ---- macros ----

    macro_invocation: ($) =>
      seq(
        field('name', $.identifier),
        '!',
        choice(
          seq('(', repeat($._token_tree), ')'),
          seq('[', repeat($._token_tree), ']'),
          seq('{', repeat($._token_tree), '}'),
        ),
      ),

    // Balanced token soup: what is inside a macro is the macro's business
    // (spec section 2.9).
    _token_tree: ($) =>
      choice(
        seq('(', repeat($._token_tree), ')'),
        seq('[', repeat($._token_tree), ']'),
        seq('{', repeat($._token_tree), '}'),
        $._token,
      ),

    _token: ($) =>
      choice(
        $.string_literal,
        $.integer_literal,
        $.boolean_literal,
        $.selector,
        $.resource_location,
        $.identifier,
        $.macro_punctuation,
      ),

    macro_punctuation: ($) =>
      token(choice(...'+-*/%=!<>&|^~?.,;:#$@'.split(''), '::', '->', '=>', '..')),

    // ---- literals and tokens ----

    identifier: ($) => /[A-Za-z_][A-Za-z0-9_]*/,

    integer_literal: ($) =>
      token(choice(/0x[0-9a-fA-F_]+/, /0b[01_]+/, /[0-9][0-9_]*/)),

    boolean_literal: ($) => choice('true', 'false'),

    string_literal: ($) => token(seq('"', repeat(choice(/[^"\\]/, /\\./)), '"')),

    // One token, brackets balanced, and a `]` inside a string does not end it
    // (spec section 2.7).
    selector: ($) =>
      token(
        seq(
          '@',
          /[aeprs]/,
          optional(seq('[', repeat(choice(/[^\[\]"]/, seq('"', repeat(/[^"]/), '"'))), ']')),
        ),
      ),

    // `minecraft:block.note_block.pling`. No space either side of the `:` is what
    // tells it from a type annotation (spec section 2.8).
    resource_location: ($) =>
      token(
        prec(
          1,
          seq(
            /[A-Za-z_][A-Za-z0-9_]*(\/[A-Za-z_][A-Za-z0-9_]*)*/,
            ':',
            /[A-Za-z0-9_.\-]+(\/[A-Za-z0-9_.\-]+)*/,
          ),
        ),
      ),
  },
});

function commaSep(rule) {
  return optional(commaSep1(rule));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}
