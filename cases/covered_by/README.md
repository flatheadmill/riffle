# Multipart CoveredBy characterization

These cases check one document with two polygon members, each inside a
different query polygon. The query rectangle has five closed positions and
the triangle has four, so Within searches the rectangle first regardless of
input order. A separate outside triangle must not match. The outside_sibling
cases move one member of the target outside both query polygons; the whole
document must then be rejected.

The literal files use longitude and latitude. Their construction in face-zero
UV coordinates is:

| Ring | Vertices before closure |
| --- | --- |
| Query rectangle | (.11,.11), (.41,.11), (.41,.41), (.11,.41) |
| Query triangle | (-.41,-.41), (-.11,-.41), (-.41,-.11) |
| Rectangle's candidate | (.21,.21), (.27,.21), (.21,.27) |
| Triangle's candidate | (-.34,-.34), (-.28,-.34), (-.34,-.28) |
| Outside candidate | (.61,.61), (.67,.61), (.61,.67) |

Plane scales u by 180 and v by 90. Sphere uses longitude atan(u) and latitude
atan(v / sqrt(1 + u*u)), in degrees. Each positive case has nine indexed edges;
each negative sibling case has six. The tests reverse query and candidate
member order independently on both surfaces.

```sh
cargo test --test predicate_within -- --nocapture
```

The test evaluates the raw reference and calls engine::run, including normal
ingestion and the Within query used by the worker binary. Its identity stamps
are synthetic test data, not a controlled build record. Output records answers
and seed diagnostics for each arrangement. The two-revision replay command
retains its existing manifest requirements; this test needs only the current
engine checkout.

The optional case field predicate defaults to intersects. These cases specify
covered_by and require longhand version covered-by-disjoint-uv-v1. This
reference admits Polygon and MultiPolygon with simple single rings in the existing
face-zero UV domain. Query members must be disjoint. Holes, overlapping or
nested query members, and boundary incidence are refused. Every document is
validated before applying the filter.

For simple rings with boundary clearance and no proper crossings, one candidate
vertex inside a query ring establishes containment of the entire candidate
member. With disjoint query members, every connected candidate member must
belong to one query member. The work matrix covered_by_query_member records
those pairwise answers in raw candidate and query order. This is a restricted
reference, not general polygon-union coverage. The separate coverage kernel is
not used here.

A passing case establishes its answer, not completeness of the covering walk.
Within finishes each query polygon's search before starting the next. The
earlier-polygon skip therefore depends on discovery completeness within a
finished pass. A positive case can assign both siblings during that first pass;
member reversal alone does not establish that a later backward dispatch ran.

## Aligned Plane edge

The aligned_right cases use a rectangle from (.25,-.2) to (.8,.8), with an
eleven-edge polygon around (.36,.31), radius .02, strictly inside it. The
multipart case adds the separated triangle query and its candidate sibling.
The single case removes both. The left_single control uses the opposite side
of the same dividing line, a rectangle from (-.3,-.2) to (.25,.8), and an
eleven-edge candidate around (.14,.31). The left side of the right rectangle
lies on the u=.25 dividing line of a level-two Plane cell. Ordinary ingestion
produces deeper indexed cells, requiring query subdivision.

All three raw answers are target. Tantivy at 9968b8ee returns no hit for any
case. The ignored regression asserts the correct answer rather than making
that failure a requirement:

```sh
cargo test --test predicate_within test_aligned_subdivision_coverage \
    -- --ignored --nocapture
```

The single-polygon failure shows that sibling dispatch is not required for
the miss. At this revision, covering subdivision uses strict comparisons to
distribute edges among child cells. With Plane's zero padding, an edge exactly
on a dividing line is assigned to neither side. These cases retain the
failure separately from the ordinary member-order checks.
