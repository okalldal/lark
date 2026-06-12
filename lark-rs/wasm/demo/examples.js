// Example bank for the playground. Grammars are taken from the repo's own
// test grammars (tests/grammars/) so the demo shows exactly what the test
// suite pins, including the in-memory `%import` mechanism (importSources).

export const EXAMPLES = [
  {
    name: "JSON",
    parser: "lalr",
    grammar: `?start: value

?value: object
      | array
      | string
      | SIGNED_NUMBER      -> number
      | "true"             -> true
      | "false"            -> false
      | "null"             -> null

array  : "[" [value ("," value)*] "]"
object : "{" [pair ("," pair)*] "}"
pair   : string ":" value

string : ESCAPED_STRING

%import common.ESCAPED_STRING
%import common.SIGNED_NUMBER
%import common.WS
%ignore WS
`,
    input: `{
  "name": "lark-rs",
  "wasm": true,
  "depth": [1, [2, [3, null]]]
}
`,
  },
  {
    name: "Arithmetic",
    parser: "lalr",
    grammar: `?start : expr

?expr  : expr "+" term  -> add
       | expr "-" term  -> sub
       | term

?term  : term "*" factor -> mul
       | term "/" factor -> div
       | factor

?factor : "+" factor    -> pos
        | "-" factor    -> neg
        | atom

?atom  : NUMBER
       | NAME
       | "(" expr ")"

%import common.NUMBER
%import common.CNAME -> NAME
%import common.WS_INLINE
%ignore WS_INLINE
`,
    input: "1 + 2 * (3 - x) / -5",
  },
  {
    name: "Hello world",
    parser: "lalr",
    grammar: `start: "hello" NAME "!"

NAME: /[A-Za-z]+/
%ignore " "
`,
    input: "hello lark!",
  },
  {
    name: "%import from a virtual file",
    parser: "lalr",
    grammar: `// "lib.lark" lives in the "Imported files" panel below the grammar —
// an in-memory file, resolved exactly like a sibling file on disk.
%import .lib (greeting)

start: greeting "!"
%ignore " "
`,
    input: "hello lark!",
    imports: {
      "lib.lark": `// Imported by the main grammar via \`%import .lib (greeting)\`.
// \`greeting\` depends on NAME, which is copied along with it.
greeting: "hello" NAME
NAME: /[a-zA-Z_]\\w*/
`,
    },
  },
  {
    name: "Ambiguity (Earley, explicit)",
    parser: "earley",
    ambiguity: "explicit",
    grammar: `// An ambiguous grammar: "1 + 2 + 3" groups two ways. With
// ambiguity = "explicit", Earley returns every derivation under _ambig
// nodes instead of picking one.
start: e
e: e "+" e
 | NUMBER

%import common.NUMBER
%import common.WS
%ignore WS
`,
    input: "1 + 2 + 3",
  },
];
