#[path = "../src/predicate/case.rs"]
mod case;
#[path = "../src/predicate/longhand.rs"]
mod longhand;

use case::{
    AmbiguousReason, Case, Document, Expected, Family, Filter, Geometry, InvalidReason, Location,
    Reason, SurfaceKind, UnsupportedReason, LONGHAND_VERSION, MIN_MARGIN,
};
use longhand::{evaluate, Answer};
use serde_json::json;

fn fixture(surface: &str, name: &str) -> Case {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases/intersects")
        .join(format!("{surface}_{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn raw(surface: SurfaceKind, [u, v]: [f64; 2]) -> [f64; 2] {
    match surface {
        SurfaceKind::Plane => [u * 180.0, v * 90.0],
        SurfaceKind::Sphere => [
            u.atan().to_degrees(),
            (v / (1.0 + u * u).sqrt()).atan().to_degrees(),
        ],
    }
}

fn point(surface: SurfaceKind, p: [f64; 2]) -> Geometry {
    Geometry::Point {
        coordinates: raw(surface, p),
    }
}

fn polygon(surface: SurfaceKind, uv: &[[f64; 2]]) -> Geometry {
    Geometry::Polygon {
        coordinates: vec![uv.iter().map(|&p| raw(surface, p)).collect()],
    }
}

fn rectangle(surface: SurfaceKind, a: f64, b: f64, c: f64, d: f64) -> Geometry {
    polygon(surface, &[[a, b], [c, b], [c, d], [a, d], [a, b]])
}

fn sample(surface: SurfaceKind, indexed: Geometry, query: Geometry) -> Case {
    Case {
        version: 1,
        name: "analytic".into(),
        predicate: case::Predicate::Intersects,
        surface,
        documents: vec![Document {
            id: "target".into(),
            geometry: indexed,
        }],
        query,
        filter: Filter::None {},
        sidecar: true,
        margin: 1e-9,
        expected: Expected {
            longhand_version: LONGHAND_VERSION.into(),
            ids: Vec::new(),
        },
    }
}

fn rejected(case: &Case, reason: Reason) {
    assert_eq!(evaluate(case).unwrap_err().reason, reason);
}

fn invalid(case: &Case, reason: InvalidReason) {
    rejected(case, Reason::Invalid { reason });
}

fn unsupported(case: &Case, reason: UnsupportedReason) {
    rejected(case, Reason::Unsupported { reason });
}

fn ambiguous(case: &Case, reason: AmbiguousReason) {
    rejected(case, Reason::Ambiguous { reason });
}

fn crossing(surface: &str) -> Case {
    fixture(surface, "later_member_crossing")
}

fn reverse(geometry: &mut Geometry) {
    match geometry {
        Geometry::Polygon { coordinates } => coordinates[0].reverse(),
        Geometry::MultiPolygon { coordinates } => {
            coordinates.reverse();
            for rings in coordinates {
                rings[0].reverse();
            }
        }
        _ => {}
    }
}

#[test]
fn test_primary_crossing_work_on_both_surfaces() {
    let mut results = Vec::new();
    for surface in ["plane", "sphere"] {
        let case = crossing(surface);
        let result = evaluate(&case).unwrap();
        assert_eq!(result.expected, case.expected.ids);
        let work = &result.work["target"];
        assert_eq!(work.crossings_by_query_member, [0, 4]);
        assert_eq!(work.indexed_vertex_in_query_member, [false, false]);
        assert_eq!(work.query_member_vertex_in_indexed, [false, false]);
        results.push(result);
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn test_primary_containment_work_on_both_surfaces() {
    let mut results = Vec::new();
    for surface in ["plane", "sphere"] {
        let case = fixture(surface, "later_member_containment");
        let result = evaluate(&case).unwrap();
        assert_eq!(result.expected, case.expected.ids);
        assert!(!result.work["decoy"].matches_without_filter);
        let work = &result.work["point"];
        assert_eq!(work.crossings_by_query_member, [0, 0]);
        assert_eq!(work.indexed_vertex_in_query_member, [false, true]);
        assert_eq!(work.query_member_vertex_in_indexed, [false, false]);
        results.push(result);
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn test_constructed_unequal_members_have_a_crossing_record() {
    for surface in ["plane", "sphere"] {
        let case = fixture(surface, "constructed_unequal_members");
        let Geometry::MultiPolygon { coordinates } = &case.query else {
            panic!()
        };
        assert_eq!(coordinates[0][0].len(), 4);
        assert_eq!(coordinates[1][0].len(), 5);
        let result = evaluate(&case).unwrap();
        assert_eq!(result.expected, case.expected.ids);
        let work = &result.work["target"];
        assert_eq!(work.crossings_by_query_member, [0, 4]);
        assert_eq!(work.indexed_vertex_in_query_member, [false, false]);
        assert_eq!(work.query_member_vertex_in_indexed, [false, false]);
        let first = Geometry::Polygon {
            coordinates: coordinates[0].clone(),
        };
        let second = Geometry::Polygon {
            coordinates: coordinates[1].clone(),
        };
        assert!(evaluate(&sample(case.surface, first, second))
            .unwrap()
            .expected
            .is_empty());
    }
}

#[test]
fn test_containment_in_both_directions_and_disjoint_points() {
    for surface in [SurfaceKind::Plane, SurfaceKind::Sphere] {
        let large = rectangle(surface, -0.5, -0.5, 0.5, 0.5);
        let small = rectangle(surface, -0.2, -0.2, 0.2, 0.2);
        for (a, b, indexed_inside, query_inside) in [
            (small.clone(), large.clone(), true, false),
            (large.clone(), small, false, true),
            (point(surface, [0.1, 0.1]), large.clone(), true, false),
            (large.clone(), point(surface, [0.1, 0.1]), false, true),
            (point(surface, [0.7, 0.7]), large.clone(), false, false),
            (large.clone(), point(surface, [0.7, 0.7]), false, false),
            (
                point(surface, [0.7, 0.7]),
                point(surface, [-0.7, -0.7]),
                false,
                false,
            ),
        ] {
            let result = evaluate(&sample(surface, a, b)).unwrap();
            let work = &result.work["target"];
            assert_eq!(work.indexed_vertex_in_query_member, [indexed_inside]);
            assert_eq!(work.query_member_vertex_in_indexed, [query_inside]);
            assert_eq!(work.matches_without_filter, indexed_inside || query_inside);
            assert_eq!(work.crossings_by_query_member, [0]);
        }
    }
}

#[test]
fn test_member_union_and_reversal() {
    for surface in [SurfaceKind::Plane, SurfaceKind::Sphere] {
        let Geometry::Polygon { coordinates: first } = rectangle(surface, -0.8, -0.7, 0.4, 0.5)
        else {
            panic!()
        };
        let Geometry::Polygon {
            coordinates: second,
        } = rectangle(surface, -0.3, -0.5, 0.8, 0.7)
        else {
            panic!()
        };
        let multi = Geometry::MultiPolygon {
            coordinates: vec![first, second],
        };
        let small = rectangle(surface, -0.1, -0.1, 0.1, 0.1);
        for (a, b) in [(small.clone(), multi.clone()), (multi, small)] {
            let mut case = sample(surface, a, b);
            let before = evaluate(&case).unwrap();
            assert_eq!(before.expected, ["target"]);
            reverse(&mut case.query);
            reverse(&mut case.documents[0].geometry);
            assert_eq!(evaluate(&case).unwrap().expected, before.expected);
        }
    }
}

#[test]
fn test_filters_preserve_unfiltered_work() {
    let mut case = crossing("plane");
    for (filter, selected) in [
        (Filter::None {}, true),
        (Filter::All {}, true),
        (
            Filter::Include {
                ids: vec!["target".into()],
            },
            true,
        ),
        (Filter::Include { ids: vec![] }, false),
        (
            Filter::Exclude {
                ids: vec!["target".into()],
            },
            false,
        ),
        (Filter::Exclude { ids: vec![] }, true),
    ] {
        case.filter = filter;
        let result = evaluate(&case).unwrap();
        assert_eq!(result.expected.len(), usize::from(selected));
        assert!(result.work["target"].matches_without_filter);
        assert_eq!(result.work["target"].selected, selected);
    }
}

#[test]
fn test_recorded_expectation_does_not_supply_the_answer() {
    let mut case = fixture("plane", "later_member_containment");
    case.expected.ids = vec!["decoy".into()];
    let result = evaluate(&case).unwrap();
    assert_eq!(result.expected, ["point"]);
    let difference = longhand::compare(&case.expected.ids, &result.expected);
    assert_eq!(difference.missing, ["decoy"]);
    assert_eq!(difference.spurious, ["point"]);
}

#[test]
fn test_round_trip_and_deterministic_result() {
    for surface in ["plane", "sphere"] {
        for name in [
            "later_member_crossing",
            "later_member_containment",
            "constructed_unequal_members",
        ] {
            let case = fixture(surface, name);
            let bytes = serde_json::to_vec(&case).unwrap();
            assert_eq!(serde_json::from_slice::<Case>(&bytes).unwrap(), case);
            let first = serde_json::to_vec(&evaluate(&case).unwrap()).unwrap();
            let second = serde_json::to_vec(&evaluate(&case).unwrap()).unwrap();
            assert_eq!(first, second);
            let result: Answer = serde_json::from_slice(&first).unwrap();
            assert_eq!(result.expected, case.expected.ids);
        }
    }
}

#[test]
fn test_document_order_does_not_change_result_serialization() {
    let mut case = fixture("plane", "later_member_containment");
    let first = serde_json::to_vec(&evaluate(&case).unwrap()).unwrap();
    case.documents.reverse();
    assert_eq!(
        serde_json::to_vec(&evaluate(&case).unwrap()).unwrap(),
        first
    );
}

#[test]
fn test_unknown_fields_are_rejected() {
    let value = serde_json::to_value(crossing("plane")).unwrap();
    for (pointer, field) in [
        ("", "radius_radians"),
        ("/query", "seed"),
        ("/filter", "deleted"),
        ("/expected", "route"),
    ] {
        let mut value = value.clone();
        value
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.into(), json!(1));
        assert!(
            serde_json::from_value::<Case>(value).is_err(),
            "accepted unknown field {field} at {pointer}"
        );
    }
    for mode in ["none", "all", "include", "exclude"] {
        let mut value = value.clone();
        value["filter"] = json!({"mode": mode, "extra": true});
        if mode == "include" || mode == "exclude" {
            value["filter"]["ids"] = json!([]);
        }
        assert!(
            serde_json::from_value::<Case>(value).is_err(),
            "mode {mode}"
        );
    }
}

#[test]
fn test_case_metadata_and_ids() {
    let valid = fixture("plane", "later_member_containment");
    let mut case = valid.clone();
    case.version = 2;
    unsupported(&case, UnsupportedReason::CaseVersion);
    case = valid.clone();
    case.expected.longhand_version = "other".into();
    unsupported(&case, UnsupportedReason::LonghandVersion);
    case = valid.clone();
    case.name.clear();
    invalid(&case, InvalidReason::EmptyName);
    case = valid.clone();
    case.documents.clear();
    invalid(&case, InvalidReason::EmptyDocuments);
    for id in ["", "point"] {
        case = valid.clone();
        case.documents[1].id = id.into();
        invalid(&case, InvalidReason::DocumentId);
    }
    for ids in [vec!["absent".into()], vec!["point".into(), "point".into()]] {
        for filter in [
            Filter::Include { ids: ids.clone() },
            Filter::Exclude { ids: ids.clone() },
        ] {
            case = valid.clone();
            case.filter = filter;
            invalid(&case, InvalidReason::FilterIds);
        }
    }
    for ids in [
        vec!["absent".into()],
        vec!["point".into(), "point".into()],
        vec!["point".into(), "decoy".into()],
    ] {
        case = valid.clone();
        case.expected.ids = ids;
        invalid(&case, InvalidReason::ExpectedIds);
    }
}

#[test]
fn test_margin_floor() {
    let mut case = crossing("plane");
    case.margin = MIN_MARGIN;
    assert!(evaluate(&case).is_ok());
    case.margin = f64::from_bits(MIN_MARGIN.to_bits() - 1);
    unsupported(&case, UnsupportedReason::MarginFloor);
    for margin in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        case.margin = margin;
        invalid(&case, InvalidReason::Margin);
    }
}

#[test]
fn test_raw_cardinality_precedes_family_gate() {
    for geometry in [
        json!({"type":"LineString", "coordinates":[[0,0]]}),
        json!({"type":"MultiLineString", "coordinates":[[[0,0],[1,1]],[[2,2]]]}),
        json!({"type":"Polygon", "coordinates":[[[0,0],[1,1],[0,0]]]}),
        json!({"type":"MultiPolygon", "coordinates":[[]]}),
        json!({"type":"MultiPoint", "coordinates":[]}),
        json!({"type":"MultiLineString", "coordinates":[]}),
        json!({"type":"MultiPolygon", "coordinates":[]}),
        json!({"type":"GeometryCollection", "geometries":[]}),
        json!({"type":"GeometryCollection", "geometries":[{"type":"LineString","coordinates":[[0,0]]}]}),
    ] {
        let mut case = crossing("plane");
        case.query = serde_json::from_value(geometry).unwrap();
        invalid(&case, InvalidReason::Cardinality);
    }
}

#[test]
fn test_unsupported_families_and_holes() {
    for geometry in [
        json!({"type":"LineString", "coordinates":[[0,0],[1,1]]}),
        json!({"type":"MultiLineString", "coordinates":[[[0,0],[1,1]]]}),
        json!({"type":"MultiPoint", "coordinates":[[0,0]]}),
        json!({"type":"GeometryCollection", "geometries":[{"type":"Point","coordinates":[0,0]}]}),
    ] {
        let mut case = crossing("plane");
        case.query = serde_json::from_value(geometry).unwrap();
        unsupported(&case, UnsupportedReason::Family);
    }
    let rings = vec![
        vec![
            [-20., -20.],
            [20., -20.],
            [20., 20.],
            [-20., 20.],
            [-20., -20.],
        ],
        vec![[-1., -1.], [1., -1.], [1., 1.], [-1., 1.], [-1., -1.]],
    ];
    for query in [
        Geometry::Polygon {
            coordinates: rings.clone(),
        },
        Geometry::MultiPolygon {
            coordinates: vec![rings],
        },
    ] {
        let mut case = crossing("plane");
        case.query = query;
        unsupported(&case, UnsupportedReason::Holes);
    }
}

#[test]
fn test_raw_coordinates_precede_family_gate() {
    for p in [[181., 0.], [0., 91.], [f64::NAN, 0.], [0., f64::INFINITY]] {
        let mut case = crossing("plane");
        case.query = Geometry::LineString {
            coordinates: vec![[0., 0.], p],
        };
        invalid(&case, InvalidReason::Coordinate);
    }
}

#[test]
fn test_face_cap_is_unsupported() {
    for surface in ["plane", "sphere"] {
        let mut case = crossing(surface);
        case.query = Geometry::Point {
            coordinates: [180., 0.],
        };
        unsupported(&case, UnsupportedReason::Face);
    }
    let mut case = crossing("plane");
    case.query = point(SurfaceKind::Plane, [0.951, 0.]);
    unsupported(&case, UnsupportedReason::Face);
}

#[test]
fn test_invalid_rings() {
    let mut case = crossing("plane");
    case.query = polygon(
        SurfaceKind::Plane,
        &[[0., 0.], [0.5, 0.], [0.5, 0.5], [0., 0.5]],
    );
    invalid(&case, InvalidReason::RingClosure);
    case.query = polygon(
        SurfaceKind::Plane,
        &[[0., 0.], [0.5, 0.], [0.5, 0.], [0., 0.5], [0., 0.]],
    );
    invalid(&case, InvalidReason::ZeroEdge);
    case.query = polygon(
        SurfaceKind::Plane,
        &[[0., 0.], [0.5, 0.], [0.7, 0.], [0., 0.]],
    );
    invalid(&case, InvalidReason::CollapsedRing);
    case.query = polygon(
        SurfaceKind::Plane,
        &[
            [-0.5, -0.5],
            [0.5, 0.5],
            [-0.5, 0.5],
            [0.5, -0.5],
            [-0.5, -0.5],
        ],
    );
    invalid(&case, InvalidReason::SelfIntersection);
}

#[test]
fn test_small_or_collinear_ring_features_are_ambiguous() {
    let mut case = crossing("plane");
    case.query = rectangle(SurfaceKind::Plane, 0., 0., 1e-10, 0.5);
    ambiguous(&case, AmbiguousReason::ShortEdge);
    case.query = polygon(
        SurfaceKind::Plane,
        &[
            [0., 0.],
            [0.25, 0.],
            [0.5, 0.],
            [0.5, 0.5],
            [0., 0.5],
            [0., 0.],
        ],
    );
    ambiguous(&case, AmbiguousReason::CollinearVertex);
    // The tip almost touches the bottom edge without making a short edge.
    case.query = polygon(
        SurfaceKind::Plane,
        &[
            [-0.5, 0.],
            [0.5, 0.],
            [0.5, 0.5],
            [0., 1e-10],
            [-0.5, 0.5],
            [-0.5, 0.],
        ],
    );
    ambiguous(&case, AmbiguousReason::RingClearance);
}

#[test]
fn test_pair_boundary_incidence_in_both_directions() {
    let surface = SurfaceKind::Plane;
    let ring = rectangle(surface, -0.5, -0.5, 0.5, 0.5);
    for (a, b) in [
        (point(surface, [0.5, 0.]), ring.clone()),
        (ring.clone(), point(surface, [0.5, 0.])),
        (ring.clone(), ring),
        (point(surface, [0., 0.]), point(surface, [0., 0.])),
    ] {
        ambiguous(&sample(surface, a, b), AmbiguousReason::BoundaryIncidence);
    }
}

#[test]
fn test_excluded_documents_still_require_complete_answer() {
    let valid = crossing("plane");
    for filter in [
        Filter::Include { ids: vec![] },
        Filter::Exclude {
            ids: vec!["target".into()],
        },
    ] {
        let mut case = valid.clone();
        case.filter = filter;
        case.documents[0].geometry = Geometry::LineString {
            coordinates: vec![[0., 0.]],
        };
        invalid(&case, InvalidReason::Cardinality);
        case.documents[0].geometry = Geometry::LineString {
            coordinates: vec![[0., 0.], [1., 1.]],
        };
        unsupported(&case, UnsupportedReason::Family);
        case.documents[0].geometry = case.query.clone();
        ambiguous(&case, AmbiguousReason::BoundaryIncidence);
    }
}

#[test]
fn test_rejection_records_stable_id_family_and_raw_location() {
    let mut case = crossing("plane");
    case.documents[0].geometry = Geometry::MultiLineString {
        coordinates: vec![vec![[0., 0.], [1., 1.]], vec![[0., 0.]]],
    };
    let rejection = evaluate(&case).unwrap_err();
    assert_eq!(rejection.family, Some(Family::MultiLineString));
    assert_eq!(
        rejection.location,
        Location::Document {
            id: "target".into(),
            path: "/coordinates/1".into()
        }
    );
    let bytes = serde_json::to_vec(&rejection).unwrap();
    assert_eq!(
        serde_json::from_slice::<case::Rejection>(&bytes).unwrap(),
        rejection
    );
}

#[test]
fn test_recorded_corpus_has_thirty_three_answered_cases() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cases/intersects");
    let corpus: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("corpus.json")).unwrap()).unwrap();
    let paths = corpus["cases"].as_array().unwrap();
    assert_eq!(paths.len(), 33);
    let mut unique = std::collections::BTreeSet::new();
    for path in paths {
        let path = path.as_str().unwrap();
        assert!(unique.insert(path));
        let case: Case = serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap();
        assert!(!case.sidecar);
        assert_eq!(
            evaluate(&case).unwrap().expected,
            case.expected.ids,
            "{path}"
        );
    }
    assert_eq!(
        corpus["calibration"]["primaries"].as_array().unwrap().len(),
        6
    );
    assert_eq!(
        corpus["calibration"]["controls"].as_array().unwrap().len(),
        8
    );
    assert_eq!(
        corpus["calibration"]["observations"]
            .as_array()
            .unwrap()
            .len(),
        19
    );
}

#[test]
fn test_recorded_controls_change_only_their_declared_dimension() {
    for surface in ["plane", "sphere"] {
        for name in [
            "later_member_crossing",
            "later_member_containment",
            "constructed_unequal_members",
        ] {
            let base = fixture(surface, name);
            let reversed = fixture(surface, &format!("{name}_reversed"));
            let mut expected = base.clone();
            expected.name = reversed.name.clone();
            let Geometry::MultiPolygon { coordinates } = &mut expected.query else {
                panic!()
            };
            coordinates.reverse();
            assert_eq!(reversed, expected);
            let empty = fixture(surface, &format!("{name}_empty_include"));
            expected = base.clone();
            expected.name = empty.name.clone();
            expected.filter = Filter::Include { ids: vec![] };
            expected.expected.ids.clear();
            assert_eq!(empty, expected);
        }
        for (suffix, filter, ids) in [
            ("all", Filter::All {}, vec!["target".to_owned()]),
            (
                "include_target",
                Filter::Include {
                    ids: vec!["target".into()],
                },
                vec!["target".into()],
            ),
            (
                "exclude_target",
                Filter::Exclude {
                    ids: vec!["target".into()],
                },
                vec![],
            ),
        ] {
            let case = fixture(surface, &format!("later_member_crossing_{suffix}"));
            let mut expected = fixture(surface, "later_member_crossing");
            expected.name = case.name.clone();
            expected.filter = filter;
            expected.expected.ids = ids;
            assert_eq!(case, expected);
        }
    }
}

#[test]
fn test_single_member_controls_exercise_the_named_cases() {
    for surface in ["plane", "sphere"] {
        for (stem, target) in [("crossing", "target"), ("containment", "point")] {
            for positive in [false, true] {
                let name = format!(
                    "single_{stem}_{}",
                    if positive { "positive" } else { "disjoint" }
                );
                let case = fixture(surface, &name);
                let base = fixture(surface, &format!("later_member_{stem}"));
                let Geometry::MultiPolygon { coordinates } = base.query else {
                    panic!()
                };
                assert_eq!(
                    case.query,
                    Geometry::Polygon {
                        coordinates: coordinates[usize::from(positive)].clone()
                    }
                );
                assert_eq!(case.documents, base.documents);
                let answer = evaluate(&case).unwrap();
                let work = &answer.work[target];
                assert_eq!(work.matches_without_filter, positive);
                assert!(work.selected);
                assert_eq!(work.query_member_vertex_in_indexed, [false]);
                assert_eq!(
                    work.crossings_by_query_member,
                    [if stem == "crossing" && positive { 4 } else { 0 }]
                );
                assert_eq!(
                    work.indexed_vertex_in_query_member,
                    [stem == "containment" && positive]
                );
            }
        }
    }
    let overlap = fixture("plane", "overlapping_members");
    let answer = evaluate(&overlap).unwrap();
    assert_eq!(answer.expected, ["target"]);
    assert_eq!(
        answer.work["target"].indexed_vertex_in_query_member,
        [true, true]
    );
}
