#[path = "../src/predicate/case.rs"]
mod case;
#[path = "../src/predicate/engine.rs"]
mod engine;
#[path = "../src/predicate/longhand.rs"]
mod longhand;
#[path = "../src/predicate/protocol.rs"]
#[allow(dead_code)]
mod protocol;

use std::fs;
use std::path::Path;

use case::{AmbiguousReason, Case, Filter, Geometry, Predicate, Reason, UnsupportedReason};
use protocol::{Completed, Diagnostics, Identity, Progress, Stage, WorkerRequest};
use serde_json::json;

fn fixture(surface: &str, name: &str) -> Case {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("cases/covered_by")
        .join(format!("{surface}_{name}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn members(geometry: &mut Geometry) -> &mut Vec<Vec<Vec<[f64; 2]>>> {
    let Geometry::MultiPolygon { coordinates } = geometry else {
        panic!("fixture must have polygon members")
    };
    coordinates
}

fn run(case: &Case) -> (Completed, Diagnostics) {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("case.json");
    let output = root.path().join("worker");
    let bytes = protocol::json_bytes(case).unwrap();
    fs::write(&input, &bytes).unwrap();
    let identity = Identity {
        protocol: protocol::PROTOCOL_VERSION,
        build_record_sha256: Some("a".repeat(64)),
        engine_revision: Some("b".repeat(40)),
    };
    let request = WorkerRequest {
        case: input.to_str().unwrap().into(),
        case_sha256: protocol::sha256(&bytes),
        build_record_sha256: identity.build_record_sha256.clone().unwrap(),
        engine_revision: identity.engine_revision.clone().unwrap(),
        output: output.to_str().unwrap().into(),
    };
    engine::run(
        request.clone(),
        vec!["within-characterization-test".into()],
        identity,
    )
    .unwrap();
    let progress: Progress =
        serde_json::from_slice(&fs::read(output.join("stage.json")).unwrap()).unwrap();
    assert_eq!(progress.stage, Stage::Result);
    assert_eq!(progress.context.identity_verified, Some(true));
    assert!(!output.join("error.json").exists());
    let completed: Completed =
        serde_json::from_slice(&fs::read(output.join("result.json")).unwrap()).unwrap();
    let diagnostics: Diagnostics =
        serde_json::from_slice(&fs::read(output.join("diagnostics.json")).unwrap()).unwrap();
    assert_eq!(completed.header, progress.context.verify().unwrap());
    assert_eq!(completed.header, diagnostics.header);
    (completed, diagnostics)
}

#[test]
fn test_sibling_coverage_in_both_member_orders() {
    for surface in ["plane", "sphere"] {
        for name in ["siblings", "outside_sibling"] {
            for reverse_query in [false, true] {
                for reverse_indexed in [false, true] {
                    let mut case = fixture(surface, name);
                    if reverse_query {
                        members(&mut case.query).reverse();
                    }
                    if reverse_indexed {
                        members(&mut case.documents[0].geometry).reverse();
                    }
                    let answer = longhand::evaluate(&case).unwrap();
                    assert_eq!(answer.expected, case.expected.ids);
                    let mut coverage = vec![vec![true, false], vec![false, name == "siblings"]];
                    if reverse_query {
                        coverage.iter_mut().for_each(|row| row.reverse());
                    }
                    if reverse_indexed {
                        coverage.reverse();
                    }
                    assert_eq!(answer.work["target"].covered_by_query_member, coverage);
                    let (result, facts) = run(&case);
                    let difference = longhand::compare(&answer.expected, &result.actual);
                    println!(
                        "{}",
                        json!({"case":case.name, "reverse_query":reverse_query,
                        "reverse_indexed":reverse_indexed, "expected":answer.expected,
                        "actual":result.actual, "cell_count":facts.cell_count, "seeds":facts.seeds})
                    );
                    assert!(difference.missing.is_empty() && difference.spurious.is_empty());
                    assert_eq!(facts.segment_count, 1);
                    assert_eq!(facts.deleted_docs, 0);
                    assert_eq!(facts.max_doc as usize, case.documents.len());
                    assert!(!facts.sidecar_used);
                    let order: Vec<_> = facts.seeds.iter().map(|seed| seed.member).collect();
                    assert_eq!(
                        order,
                        if reverse_query {
                            vec![0, 1]
                        } else {
                            vec![1, 0]
                        }
                    );
                    for seed in &facts.seeds {
                        assert_eq!(seed.member_vertices, seed.member_zero_vertices);
                        assert_eq!(seed.edge_indices.len(), seed.member_vertices - 1);
                        assert!(seed.index_start < seed.index_end);
                    }
                }
            }
        }
    }
}

#[test]
fn test_reference_requires_containment_without_crossing() {
    let mut case = fixture("plane", "siblings");
    case.documents.truncate(1);
    for (ring, expected) in [
        (vec![[0.2, 0.2], [0.3, 0.2], [0.2, 0.3], [0.2, 0.2]], true),
        (vec![[0.2, 0.2], [0.5, 0.2], [0.2, 0.3], [0.2, 0.2]], false),
        (
            vec![
                [0.01, 0.01],
                [0.51, 0.01],
                [0.51, 0.51],
                [0.01, 0.51],
                [0.01, 0.01],
            ],
            false,
        ),
    ] {
        case.documents[0].geometry = Geometry::Polygon {
            coordinates: vec![ring
                .into_iter()
                .map(|[u, v]| [u * 180.0, v * 90.0])
                .collect()],
        };
        let answer = longhand::evaluate(&case).unwrap();
        assert_eq!(answer.work["target"].matches_without_filter, expected);
    }
}

#[test]
fn test_reference_refuses_overlapping_and_incident_query_members() {
    for (bounds, reason) in [
        (
            [0.21, 0.21, 0.51, 0.51],
            Reason::Unsupported {
                reason: UnsupportedReason::QueryMembersOverlap,
            },
        ),
        (
            [0.21, 0.21, 0.31, 0.31],
            Reason::Unsupported {
                reason: UnsupportedReason::QueryMembersOverlap,
            },
        ),
        (
            [0.41, 0.21, 0.51, 0.31],
            Reason::Ambiguous {
                reason: AmbiguousReason::BoundaryIncidence,
            },
        ),
    ] {
        let mut case = fixture("plane", "siblings");
        let [a, b, c, d] = bounds;
        members(&mut case.query)[0] = vec![vec![[a, b], [c, b], [c, d], [a, d], [a, b]]
            .into_iter()
            .map(|[u, v]| [u * 180.0, v * 90.0])
            .collect()];
        assert_eq!(longhand::evaluate(&case).unwrap_err().reason, reason);
    }
}

#[test]
fn test_predicate_admission_and_excluded_documents() {
    let mut case = fixture("plane", "siblings");
    case.predicate = Predicate::Intersects;
    assert_eq!(
        longhand::evaluate(&case).unwrap_err().reason,
        Reason::Unsupported {
            reason: UnsupportedReason::LonghandVersion
        }
    );
    case.predicate = Predicate::CoveredBy;
    case.filter = Filter::Include { ids: vec![] };
    assert!(longhand::evaluate(&case).unwrap().expected.is_empty());
    assert!(run(&case).0.actual.is_empty());
    case.documents[1].geometry = Geometry::Point {
        coordinates: [0.0, 0.0],
    };
    assert_eq!(
        longhand::evaluate(&case).unwrap_err().reason,
        Reason::Unsupported {
            reason: UnsupportedReason::Family
        }
    );
}

#[test]
fn test_aligned_reference_coverage() {
    for name in [
        "aligned_right",
        "aligned_right_single",
        "aligned_left_single",
    ] {
        let case = fixture("plane", name);
        let answer = longhand::evaluate(&case).unwrap();
        assert_eq!(answer.expected, ["target"]);
        assert_eq!(answer.expected, case.expected.ids);
        assert!(answer.work["target"]
            .crossings_by_query_member
            .iter()
            .all(|&count| count == 0));
    }
}

#[test]
fn test_aligned_subdivision_coverage() {
    let mut differences = Vec::new();
    for name in [
        "aligned_right",
        "aligned_right_single",
        "aligned_left_single",
    ] {
        let case = fixture("plane", name);
        let answer = longhand::evaluate(&case).unwrap();
        let (result, facts) = run(&case);
        println!(
            "{}",
            json!({"case":case.name, "expected":answer.expected,
            "actual":result.actual, "diagnostics":facts})
        );
        assert!(facts.seeds.iter().any(|seed| seed
            .first_index_level
            .is_some_and(|level| level > seed.query_level)));
        if result.actual != answer.expected {
            differences.push((case.name, result.actual, answer.expected));
        }
    }
    assert!(differences.is_empty(), "{differences:?}");
}
