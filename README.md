# Riffle

Riffle supplies spatial indexing, queries and joins through Tantivy's segment
plugin and query interfaces.

The current dependencies use fixed Git revisions. Tantivy and `tantivy-common`
come from the same revision; `tantivy-spatial` comes from a separate revision of
the same repository. Use `Cargo.lock` and `cargo build --locked` to retain the
resolved dependency graph.

Applications that depend on Riffle must add this temporary patch to their own
workspace root `Cargo.toml`:

```toml
[patch.crates-io]
tantivy-common = { git = "https://github.com/flatheadmill/tantivy.git", rev = "37bef7c463300acd5882699cff3e05bfd81dc8e5" }
```

Cargo ignores patches in dependencies. This patch makes the spatial crate use
the same common types as Tantivy and Riffle. An application that also depends on
Tantivy directly must use the same URL and revision shown above. Matching only
the package version does not preserve Rust type identity across sources.

Register the spatial plugin before constructing writers. After reopening an
index, `open_opaque_index` attaches the plugin again. The spatial query and join
interfaces are in `riffle::query`; the examples show document input and queries.
