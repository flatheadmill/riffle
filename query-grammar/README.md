# Riffle Query Grammar

This crate parses Riffle queries into a recursive AST. It derives from
Tantivy query-grammar at commit `14cde3289`. The original MIT notice and
author list are retained in this crate.

Spatial joins accept a query argument, for example
`geometry:$within(1km, $query(kind:trail AND open:true))`. The `inner_query`
field of `UserInputLeaf::Spatial` contains `Option<Box<UserInputAst>>`.
Callers can inspect and modify that child before building an executable query.
It is serialized as a nested AST object; earlier versions stored query text.
Literal geometry has `inner_query: None`, serialized as `null`.

Both parser entry points parse query arguments recursively. Strict parsing
accepts whitespace between a function name and its opening parenthesis and
rejects malformed spatial calls. Lenient parsing reports errors and preserves
the inner query when its boundary can be identified. If invalid parameters or
an unfinished expression prevent finding that boundary, it discards the
malformed call and the remaining input. When only the call's closing parenthesis
is missing after a complete query argument, later clauses can still be parsed.

The grammar recognizes spatial syntax independently of execution support.
The query engine decides which predicates are supported by each surface.

Rewriting descends through boosts and nested spatial queries. Repeated
clauses inside a boosted group are deduplicated. This preserves the
extended grammar's behavior and intentionally differs from Tantivy's
ordinary grammar, whose rewrite does not descend through boosts.
