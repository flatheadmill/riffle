# Closed-region coverage kernel

`src/predicate/coverage.rs` tests two-dimensional interior coverage in one
face's projected UV coordinates. It is an independent module with a Cargo test
target. Intersects replay does not use it, and it does not implement complete
Contains or Within predicates.

A Point is `[x, y]`, a Ring is a closed sequence of points, a Polygon is a
sequence of rings, and a Region is a sequence of polygons. Rings within a
polygon combine by parity; polygons combine by union. Callers must validate
simple rings with nonzero area and valid hole ownership, including no nested
holes. Winding and member order do not change the represented region.

The kernel requires finite coordinates with absolute value below one, a finite
margin of at least 1e-12, and every edge longer than the margin. Edge length uses
`hypot`; the floor prevents squared-length underflow in point-to-edge distance.
The raw-case longhand calculation uses the same engineering floor. This module has no case or
engine dependency.

Malformed rings, empty polygons, invalid coordinates or margins, short edges,
boundary incidence and inadequate input clearance are errors. An empty Region
represents the empty set. Both empty regions produce Covered with zero samples.
Validation still precedes arrangement work when either region is empty.

The sweep retains distinct x coordinates of vertices and proper edge
crossings without tolerance merging. An intersection event with a vertical
edge takes that edge's x coordinate exactly. The sweep samples every
enumerated open slab and y interval. Unrepresentable midpoints, equal y
values and inadequate sample clearance count as unsafe samples. A safe
subject-interior point outside the container yields NotCovered, even if
another sample was unsafe. Otherwise unsafe samples yield Ambiguous; Covered
requires every enumerated sample to be safe. The sample count includes slabs
that have no representable midpoint.

This uses ordinary f64 arrangement arithmetic, so it is not an exact-arithmetic
certificate for adversarial near-degenerate inputs. Endpoint clearance does not
bound separation among intersection vertices. A missed event can remove the
interval that would have exposed it; sample clearance cannot prove complete
event enumeration. Boundary-only and open-member obligations need separate
checks.

Run the kernel tests with:

```sh
cargo test --test predicate_coverage
```

The nine geometric constructions cover member unions, enclosed holes, internal
member boundaries, concave notches, winding and member order, inadequate sample
clearance, diagonal boundaries, disconnected subjects and coincident boundaries.
Refusal and empty-region tests state the admission limits. Each fixture pair is
checked under all eight signed axis maps. Expected verdict kinds and refusal
messages stay fixed; sample counts and point coordinates may differ.
