#[path = "../src/predicate/calibration.rs"]
mod calibration;
#[path = "../src/predicate/case.rs"]
mod case;
#[path = "../src/predicate/longhand.rs"]
mod longhand;
#[path = "../src/predicate/protocol.rs"]
#[allow(dead_code)]
mod protocol;
#[path = "../src/predicate/runner.rs"]
mod runner;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use calibration::{AgreementOutcome, Calibration, Mechanism, Reason, Verdict};
use case::{Case, Geometry, SurfaceKind};
use protocol::{Context, Identity, Observed, Progress, Stage, WorkerRequest};
use runner::{CaseObservation, ReferenceOutcome, RunReport, WorkerObservation};
use serde_json::json;

fn process(code: i32) -> runner::ProcessObservation {
    runner::ProcessObservation {
        invocation: runner::Invocation {
            argv: vec!["synthetic-worker".into()],
            cwd: "/synthetic".into(),
            environment: "inherited".into(),
            timeout_ms: 1000,
        },
        elapsed_ms: 1,
        timed_out: false,
        kill_requested: false,
        kill_error: None,
        waited: Some(runner::WaitStatus {
            code: Some(code),
            signal: None,
        }),
        orchestration_error: None,
    }
}

fn verified(revision: &str, digit: &str) -> runner::VerifiedWorker {
    let identity = Identity {
        protocol: 1,
        build_record_sha256: Some(digit.repeat(64)),
        engine_revision: Some(revision.into()),
    };
    runner::VerifiedWorker {
        entry: runner::WorkerEntry {
            executable: "synthetic-worker".into(),
            executable_sha256: digit.repeat(64),
            build_record: "synthetic-record.json".into(),
            build_record_sha256: digit.repeat(64),
            engine_revision: revision.into(),
        },
        executable: "/synthetic/worker".into(),
        build_record: "/synthetic/record.json".into(),
        identity,
        identity_process: process(0),
    }
}

fn worker(case: &Case, answer: &longhand::Answer, identity: &Identity) -> WorkerObservation {
    let context = Context {
        requested: WorkerRequest {
            case: "/synthetic/case.json".into(),
            case_sha256: "a".repeat(64),
            build_record_sha256: identity.build_record_sha256.clone().unwrap(),
            engine_revision: identity.engine_revision.clone().unwrap(),
            output: "/synthetic/output".into(),
        },
        compiled: identity.clone(),
        observed: Observed {
            case_sha256: Some("a".repeat(64)),
        },
        identity_verified: Some(true),
        argv: vec!["synthetic-worker".into()],
    };
    let header = context.verify().unwrap();
    let members: Vec<_> = match &case.query {
        Geometry::Polygon { coordinates } => vec![coordinates[0].len()],
        Geometry::MultiPolygon { coordinates } => coordinates.iter().map(|m| m[0].len()).collect(),
        _ => unreachable!(),
    };
    let seeds: Vec<_> = members
        .iter()
        .enumerate()
        .map(|(member, &vertices)| protocol::Seed {
            cell_id: format!("synthetic-{member}"),
            query_level: 0,
            member: member as u32,
            contains_center: false,
            edge_indices: (0..(vertices - 1) as u32).collect(),
            member_vertices: vertices,
            member_zero_vertices: members[0],
            index_start: 0,
            index_end: 1,
            first_index_level: Some(if case.name.contains("containment") {
                0
            } else {
                1
            }),
        })
        .collect();
    WorkerObservation {
        process: process(0),
        protocol_issues: Vec::new(),
        progress: Some(Progress {
            context,
            stage: Stage::Result,
        }),
        error: None,
        semantic: Some(runner::SemanticComparison {
            actual: answer.expected.clone(),
            missing: Vec::new(),
            spurious: Vec::new(),
        }),
        diagnostics: runner::DiagnosticObservation {
            facts: Some(protocol::Diagnostics {
                header,
                segment_count: 1,
                segment_id: "synthetic-segment".into(),
                max_doc: case.documents.len() as u32,
                deleted_docs: 0,
                stored_ids: case
                    .documents
                    .iter()
                    .enumerate()
                    .map(|(index, doc)| protocol::StoredId {
                        doc_id: index as u32,
                        id: doc.id.clone(),
                    })
                    .collect(),
                filter: case.filter.clone(),
                selected_ids: case
                    .documents
                    .iter()
                    .filter(|d| case.filter.selected(&d.id))
                    .map(|d| d.id.clone())
                    .collect(),
                sidecar_requested: false,
                sidecar_available: false,
                sidecar_used: false,
                cell_count: 1,
                clipper: protocol::ClipperSettings {
                    max_edges_per_cell: 10,
                    min_short_edge_fraction: 0.2,
                },
                seeds,
            }),
            issues: Vec::new(),
        },
        panic: None,
        stdout_excerpt: None,
        stderr: String::new(),
        agreement: true,
    }
}

// These reports exercise classification, not ingestion or historical behavior.
fn fixture() -> RunReport {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("cases/intersects");
    let corpus: runner::Corpus =
        serde_json::from_slice(&fs::read(root.join("corpus.json")).unwrap()).unwrap();
    let plan = corpus.calibration.as_ref().unwrap();
    let baseline = verified(&plan.baseline_revision, "b");
    let repaired = verified(&plan.repaired_revision, "c");
    let cases = corpus
        .cases
        .iter()
        .map(|path| {
            let bytes = fs::read(root.join(path)).unwrap();
            let case: Case = serde_json::from_slice(&bytes).unwrap();
            let answer = longhand::evaluate(&case).unwrap();
            let mut baseline = worker(&case, &answer, &baseline.identity);
            let repaired = worker(&case, &answer, &repaired.identity);
            if let Some(primary) = plan.primaries.iter().find(|p| &p.path == path) {
                baseline.agreement = false;
                if primary.mechanism == Mechanism::Split {
                    baseline.process = process(101);
                    baseline.progress.as_mut().unwrap().stage = Stage::Query;
                    baseline.semantic = None;
                    baseline.panic = Some(runner::PanicEvidence {
                        thread: "main".into(),
                        location: "src/query_edge_provider.rs:42:9".into(),
                        message: "index out of bounds: the len is 4 but the index is 4".into(),
                    });
                } else {
                    baseline.semantic = Some(runner::SemanticComparison {
                        actual: answer
                            .expected
                            .iter()
                            .filter(|id| *id != &primary.target)
                            .cloned()
                            .collect(),
                        missing: vec![primary.target.clone()],
                        spurious: Vec::new(),
                    });
                }
            }
            CaseObservation {
                source: format!("/synthetic/{path}"),
                preserved: Some(format!("/synthetic/preserved/{path}")),
                case_sha256: Some(protocol::sha256(&bytes)),
                case: Some(case),
                reference: ReferenceOutcome::Answered { answer },
                baseline: Some(baseline),
                repaired: Some(repaired),
            }
        })
        .collect();
    RunReport {
        version: 1,
        invocation: process(0).invocation,
        input: "/synthetic/corpus.json".into(),
        manifest: "/synthetic/workers.json".into(),
        canonical_input: Some("/synthetic/corpus.json".into()),
        canonical_manifest: Some("/synthetic/workers.json".into()),
        corpus: Some(corpus),
        reference: Some(runner::ReferenceIdentity {
            executable: repaired.executable.clone(),
            executable_sha256: repaired.entry.executable_sha256.clone(),
            compiled: repaired.identity.clone(),
        }),
        workers: Some(runner::VerifiedWorkers {
            reference: repaired.clone(),
            baseline,
            repaired,
        }),
        cases,
        complete: true,
        reference_failed: false,
        error: None,
    }
}

fn evaluate(report: &RunReport) -> Calibration {
    calibration::evaluate(report).unwrap().unwrap()
}

fn both_facts(observation: &mut CaseObservation, change: impl Fn(&mut protocol::Diagnostics)) {
    for worker in [&mut observation.baseline, &mut observation.repaired] {
        change(worker.as_mut().unwrap().diagnostics.facts.as_mut().unwrap());
    }
}

#[test]
fn test_complete_matrix_and_report_digest() {
    let report = fixture();
    let result = evaluate(&report);
    assert!(result.qualified);
    assert_eq!(result.primaries.len(), 6);
    assert_eq!(result.controls.len(), 8);
    assert_eq!(result.repaired.len(), 33);
    assert!(result
        .primaries
        .iter()
        .all(|p| p.verdict == Verdict::Reproduced));
    assert!(result.reasons.is_empty());
    let temp = tempfile::tempdir().unwrap();
    protocol::write_json(&temp.path().join("report.json"), &report).unwrap();
    assert_eq!(result.report, "report.json");
    assert_eq!(
        result.report_sha256,
        protocol::sha256(&fs::read(temp.path().join("report.json")).unwrap())
    );
    assert_eq!(result, evaluate(&report));
    assert_eq!(
        result.reference_revision,
        report.reference.as_ref().unwrap().compiled.engine_revision
    );
}

#[test]
fn test_calibration_refuses_other_predicates() {
    for index in [0, 6, 14] {
        let mut report = fixture();
        report.cases[index].case.as_mut().unwrap().predicate = case::Predicate::CoveredBy;
        let result = evaluate(&report);
        assert!(!result.qualified);
        assert!(result
            .repaired
            .iter()
            .any(|case| case.reasons.contains(&Reason::PredicateMismatch)));
    }
}

#[test]
fn test_plan_requires_matrices_paths_and_actual_revisions() {
    let mut report = fixture();
    let plan = report
        .corpus
        .as_mut()
        .unwrap()
        .calibration
        .as_mut()
        .unwrap();
    plan.version = 2;
    plan.baseline_revision = "d".repeat(40);
    plan.repaired_revision = "e".repeat(40);
    plan.primaries[0].surface = SurfaceKind::Sphere;
    plan.controls[0].kind = calibration::ControlKind::CrossingDisjoint;
    plan.observations[0].path = "../other.json".into();
    plan.observations.push(plan.observations[1].clone());
    let result = evaluate(&report);
    assert_eq!(
        result.reasons,
        BTreeSet::from([
            Reason::ControlMatrix,
            Reason::CorpusMismatch,
            Reason::DuplicatePlanPath,
            Reason::InvalidPlanPath,
            Reason::PlanVersion,
            Reason::PrimaryMatrix,
            Reason::ReferenceRevisionMismatch,
            Reason::RevisionMismatch,
        ])
    );
    assert!(result
        .primaries
        .iter()
        .all(|p| p.verdict == Verdict::Inconclusive));
    assert!(result.primaries[0].evidence.observed_pattern);

    let mut report = fixture();
    report.reference.as_mut().unwrap().compiled.engine_revision = Some("f".repeat(40));
    let result = evaluate(&report);
    assert_eq!(
        result.reasons,
        BTreeSet::from([Reason::ReferenceRevisionMismatch])
    );
    assert!(!result.qualified);
}

#[test]
fn test_observation_count_can_grow_without_changing_obligations() {
    let mut report = fixture();
    let mut case = report.cases.last().unwrap().clone();
    case.source = "/synthetic/additional.json".into();
    report.cases.push(case);
    let corpus = report.corpus.as_mut().unwrap();
    corpus.cases.push("additional.json".into());
    corpus
        .calibration
        .as_mut()
        .unwrap()
        .observations
        .push(calibration::Observation {
            path: "additional.json".into(),
            surface: SurfaceKind::Plane,
        });
    assert!(evaluate(&report).qualified);
    report
        .corpus
        .as_mut()
        .unwrap()
        .cases
        .push("additional.json".into());
    assert!(evaluate(&report).reasons.contains(&Reason::CorpusMismatch));
}

#[test]
fn test_missing_and_unanswered_cases_do_not_cascade() {
    let mut report = fixture();
    report.cases.remove(0);
    let result = evaluate(&report);
    assert_eq!(
        result.primaries[0].reasons,
        BTreeSet::from([Reason::CaseMissing])
    );
    assert_eq!(result.primaries[0].verdict, Verdict::Inconclusive);
    assert!(!result.surfaces[0].cases_consistent);
    assert!(result.surfaces[1].qualified);
    for reference in [
        ReferenceOutcome::Unreadable {
            message: "absent".into(),
        },
        ReferenceOutcome::Malformed {
            message: "bad JSON".into(),
        },
        ReferenceOutcome::Failed {
            message: "longhand fault".into(),
        },
    ] {
        let mut report = fixture();
        report.cases[0].reference = reference;
        report.cases[0].baseline = None;
        report.cases[0].repaired = None;
        let result = evaluate(&report);
        assert_eq!(
            result.primaries[0].reasons,
            BTreeSet::from([Reason::CaseUnanswered])
        );
        assert_eq!(
            result.repaired[0].reasons,
            BTreeSet::from([Reason::CaseUnanswered])
        );
    }
}

#[test]
fn test_control_shapes_and_agreement_gate_their_own_surface() {
    for shape_failure in [false, true] {
        let mut report = fixture();
        let control = report
            .cases
            .iter_mut()
            .find(|c| c.source.ends_with("plane_single_containment_positive.json"))
            .unwrap();
        if shape_failure {
            let case = control.case.as_mut().unwrap();
            case.query = Geometry::MultiPolygon {
                coordinates: match &case.query {
                    Geometry::Polygon { coordinates } => {
                        vec![coordinates.clone(), coordinates.clone()]
                    }
                    _ => unreachable!(),
                },
            };
        } else {
            control
                .baseline
                .as_mut()
                .unwrap()
                .semantic
                .as_mut()
                .unwrap()
                .missing = vec!["point".into()];
        }
        let result = evaluate(&report);
        assert!(!result.controls[2].passed);
        assert!(result.primaries[..3]
            .iter()
            .all(|p| p.reasons.contains(&Reason::ControlFailed)));
        assert!(result.primaries[0].evidence.observed_pattern);
        assert!(result.surfaces[1].qualified);
        if shape_failure {
            assert!(result.controls[2]
                .reasons
                .contains(&Reason::ControlShapeMismatch));
        } else {
            assert_eq!(
                result.controls[2].baseline.outcome,
                AgreementOutcome::Disagrees
            );
        }
    }
}

#[test]
fn test_same_member_work_and_seed_are_required() {
    let mut report = fixture();
    let observation = &mut report.cases[0];
    let case = observation.case.as_mut().unwrap();
    if let Geometry::MultiPolygon { coordinates } = &mut case.query {
        coordinates.insert(1, coordinates[0].clone());
    }
    observation.reference = ReferenceOutcome::Answered {
        answer: longhand::evaluate(case).unwrap(),
    };
    let result = evaluate(&report);
    assert_eq!(result.primaries[0].evidence.raw_members, [2]);
    assert_eq!(
        result.primaries[0].reasons,
        BTreeSet::from([Reason::SeedIneligible])
    );
    assert!(result.primaries[0].evidence.observed_pattern);
    both_facts(&mut report.cases[0], |facts| facts.seeds[1].member = 2);
    assert!(evaluate(&report).qualified);
}

#[test]
fn test_each_mechanism_requires_its_seed_preconditions() {
    let changes: [(usize, fn(&mut protocol::Diagnostics)); 5] = [
        (0, |f| f.seeds[1].first_index_level = Some(0)),
        (0, |f| f.seeds[1].edge_indices.clear()),
        (1, |f| f.seeds[1].first_index_level = Some(1)),
        (2, |f| f.seeds[1].edge_indices = vec![0]),
        (2, |f| {
            f.seeds[1].index_start = 1;
            f.seeds[1].first_index_level = None;
        }),
    ];
    for (index, change) in changes {
        let mut report = fixture();
        both_facts(&mut report.cases[index], change);
        let result = evaluate(&report);
        assert_eq!(
            result.primaries[index].reasons,
            BTreeSet::from([Reason::SeedIneligible])
        );
        assert_eq!(result.primaries[index].verdict, Verdict::Inconclusive);
    }
    let mut report = fixture();
    both_facts(&mut report.cases[2], |facts| {
        facts.seeds[1].member_vertices = 4
    });
    assert!(evaluate(&report).primaries[2]
        .reasons
        .contains(&Reason::DiagnosticsShapeMismatch));
}

#[test]
fn test_diagnostic_consistency_checks_seeds_clipper_cells_and_sidecar() {
    for field in ["seeds", "clipper", "cell_count", "sidecar_available"] {
        let mut report = fixture();
        let facts = report.cases[0]
            .repaired
            .as_mut()
            .unwrap()
            .diagnostics
            .facts
            .as_mut()
            .unwrap();
        match field {
            "seeds" => facts.seeds[0].contains_center = true,
            "clipper" => facts.clipper.max_edges_per_cell += 1,
            "cell_count" => facts.cell_count += 1,
            _ => facts.sidecar_available = true,
        }
        let result = evaluate(&report);
        assert!(result.primaries[0]
            .reasons
            .contains(&Reason::DiagnosticsDivergence));
        assert_eq!(
            result.repaired[0].agreement.outcome,
            AgreementOutcome::Agrees
        );
        assert!(result.surfaces[1].qualified);
    }
    let mut report = fixture();
    report.cases[0]
        .repaired
        .as_mut()
        .unwrap()
        .diagnostics
        .facts
        .as_mut()
        .unwrap()
        .segment_id = "another-segment".into();
    assert!(evaluate(&report).qualified);
    // Doc IDs are recovered per worker; their permutation is not a shape change.
    let facts = report.cases[1]
        .repaired
        .as_mut()
        .unwrap()
        .diagnostics
        .facts
        .as_mut()
        .unwrap();
    facts.stored_ids[0].doc_id = 1;
    facts.stored_ids[1].doc_id = 0;
    assert!(evaluate(&report).qualified);
    report.cases[0].baseline.as_mut().unwrap().diagnostics.facts = None;
    let result = evaluate(&report);
    assert!(result.primaries[0]
        .reasons
        .contains(&Reason::DiagnosticsUnavailable));
    assert!(!result.primaries[0]
        .reasons
        .contains(&Reason::DiagnosticsDivergence));
}

#[test]
fn test_misses_mixed_differences_and_completed_split() {
    let mut report = fixture();
    report.cases[0]
        .baseline
        .as_mut()
        .unwrap()
        .semantic
        .as_mut()
        .unwrap()
        .spurious
        .push("extra".into());
    let result = evaluate(&report);
    assert_eq!(result.primaries[0].verdict, Verdict::Inconclusive);
    assert!(result.primaries[0]
        .reasons
        .contains(&Reason::UnexpectedDifference));
    assert_eq!(
        result.primaries[0]
            .evidence
            .baseline_difference
            .as_ref()
            .unwrap()
            .missing,
        ["target"]
    );
    report.cases[0].baseline = report.cases[0].repaired.clone();
    assert_eq!(
        evaluate(&report).primaries[0].verdict,
        Verdict::NotReproduced
    );
    report.cases[2].baseline = report.cases[2].repaired.clone();
    report.cases[2]
        .baseline
        .as_mut()
        .unwrap()
        .semantic
        .as_mut()
        .unwrap()
        .missing
        .push("target".into());
    let result = evaluate(&report);
    assert_eq!(result.primaries[2].verdict, Verdict::NotReproduced);
    assert!(result.primaries[2].reasons.is_empty());
}

#[test]
fn test_panic_stage_thread_message_and_source_route() {
    let changes: [(fn(&mut WorkerObservation), Reason); 5] = [
        (
            |w| w.panic.as_mut().unwrap().location = "src/intersects.rs:227:3".into(),
            Reason::UnexpectedRoute,
        ),
        (
            |w| w.progress.as_mut().unwrap().stage = Stage::Index,
            Reason::UnexpectedPanic,
        ),
        (
            |w| w.panic.as_mut().unwrap().thread = "background".into(),
            Reason::UnexpectedPanic,
        ),
        (
            |w| w.panic.as_mut().unwrap().message = "other panic".into(),
            Reason::UnexpectedPanic,
        ),
        (
            |w| {
                w.panic = None;
                w.process = process(1);
            },
            Reason::WorkerExit,
        ),
    ];
    for (change, reason) in changes {
        let mut report = fixture();
        change(report.cases[2].baseline.as_mut().unwrap());
        let result = evaluate(&report);
        assert_eq!(result.primaries[2].verdict, Verdict::Inconclusive);
        assert!(result.primaries[2].reasons.contains(&reason));
    }
}

#[test]
fn test_split_bounds_panic_is_bound_to_an_eligible_edge() {
    let report = fixture();
    assert_eq!(
        evaluate(&report).primaries[2].evidence.panic_bounds,
        Some(calibration::PanicBounds { len: 4, index: 4 })
    );
    for message in [
        "index out of bounds: the len is 2 but the index is 2",
        "index out of bounds: the len is 4 but the index is 5",
        "index out of bounds: the len is 4 but the index is 3",
        "index out of bounds: the len is 4 but the index is +4",
        "index out of bounds: the len is 4 but the index is 4 extra",
        "index out of bounds: the len is 4 but the index is 18446744073709551616",
    ] {
        let mut report = report.clone();
        report.cases[2]
            .baseline
            .as_mut()
            .unwrap()
            .panic
            .as_mut()
            .unwrap()
            .message = message.into();
        let result = evaluate(&report);
        assert_eq!(
            result.primaries[2].reasons,
            BTreeSet::from([Reason::PanicUnbound])
        );
        assert_eq!(result.primaries[2].verdict, Verdict::Inconclusive);
        assert!(!result.primaries[2].evidence.observed_pattern);
    }
    let mut report = report;
    let observation = &mut report.cases[2];
    let case = observation.case.as_mut().unwrap();
    if let Geometry::MultiPolygon { coordinates } = &mut case.query {
        coordinates[1][0].splice(4..4, [[34.2, 31.8], [34.2, 31.2]]);
    }
    observation.reference = ReferenceOutcome::Answered {
        answer: longhand::evaluate(case).unwrap(),
    };
    both_facts(observation, |facts| {
        facts.seeds[1].member_vertices = 7;
        facts.seeds[1].edge_indices = vec![5];
    });
    // Edge 5 fails on its first endpoint. Its second endpoint cannot be read.
    for (index, verdict) in [(5, Verdict::Reproduced), (6, Verdict::Inconclusive)] {
        report.cases[2]
            .baseline
            .as_mut()
            .unwrap()
            .panic
            .as_mut()
            .unwrap()
            .message = format!(
            "index out of bounds: the len is 4 but the index is {index}\nnote: backtrace follows"
        );
        let result = evaluate(&report);
        assert_eq!(result.primaries[2].verdict, verdict);
        assert_eq!(
            result.primaries[2].evidence.panic_bounds,
            Some(calibration::PanicBounds { len: 4, index })
        );
    }
}

#[test]
fn test_missing_prerequisites_do_not_add_dependent_reasons() {
    let mut report = fixture();
    report.workers = None;
    report.reference = None;
    assert_eq!(
        evaluate(&report).reasons,
        BTreeSet::from([Reason::BuildsUnavailable])
    );
    report.reference = fixture().reference;
    report.reference.as_mut().unwrap().compiled.engine_revision = Some("d".repeat(40));
    assert_eq!(
        evaluate(&report).reasons,
        BTreeSet::from([Reason::BuildsUnavailable, Reason::ReferenceRevisionMismatch])
    );

    let mut report = fixture();
    let observation = &mut report.cases[0];
    let case = observation.case.as_mut().unwrap();
    if let Geometry::MultiPolygon { coordinates } = &mut case.query {
        coordinates.reverse();
    }
    observation.reference = ReferenceOutcome::Answered {
        answer: longhand::evaluate(case).unwrap(),
    };
    let result = evaluate(&report);
    assert_eq!(
        result.primaries[0].reasons,
        BTreeSet::from([Reason::WorkMismatch])
    );
    assert!(result.primaries[0].evidence.eligible_seeds.is_empty());
}

#[test]
fn test_independent_failures_are_collected_and_repair_is_separate() {
    let mut report = fixture();
    let worker = report.cases[0].baseline.as_mut().unwrap();
    worker.process.timed_out = true;
    worker.protocol_issues.push(runner::Issue {
        kind: runner::IssueKind::UnexpectedStdout,
        record: "stdout".into(),
        message: "noise".into(),
    });
    worker.diagnostics.facts = None;
    report
        .workers
        .as_mut()
        .unwrap()
        .baseline
        .entry
        .engine_revision = "d".repeat(40);
    let result = evaluate(&report);
    assert_eq!(
        result.primaries[0].reasons,
        BTreeSet::from([
            Reason::DiagnosticsUnavailable,
            Reason::ProtocolViolation,
            Reason::RevisionMismatch,
            Reason::WorkerTimeout,
        ])
    );

    let mut report = fixture();
    report.cases[0]
        .repaired
        .as_mut()
        .unwrap()
        .semantic
        .as_mut()
        .unwrap()
        .missing = vec!["target".into()];
    let result = evaluate(&report);
    assert_eq!(result.primaries[0].verdict, Verdict::Reproduced);
    assert_eq!(
        result.repaired[0].agreement.outcome,
        AgreementOutcome::Disagrees
    );
    assert!(!result.surfaces[0].qualified);
    assert!(result.surfaces[1].qualified);
}

#[test]
fn test_optional_and_malformed_plans_and_preflight_capture() {
    let mut report = fixture();
    report.corpus.as_mut().unwrap().calibration = None;
    assert!(calibration::evaluate(&report).unwrap().is_none());
    report.corpus = None;
    assert!(calibration::evaluate(&report).unwrap().is_none());

    let source = fixture().corpus.unwrap();
    let mut value = serde_json::to_value(&source).unwrap();
    value["calibration"]["primaries"][0]["mechanism"] = json!("unknown");
    assert!(runner::replay_input(
        Path::new("/synthetic/corpus.json"),
        &serde_json::to_vec(&value).unwrap()
    )
    .is_err());

    let root = tempfile::tempdir().unwrap();
    protocol::write_json(&root.path().join("corpus.json"), &source).unwrap();
    let request = runner::ReplayRequest {
        input: root.path().join("corpus.json"),
        workers: root.path().join("absent.json"),
        output: root.path().join("run"),
        timeout_seconds: 1,
    };
    assert_eq!(
        runner::replay(request, vec!["synthetic-replay".into()]).unwrap(),
        1
    );
    let bytes = fs::read(root.path().join("run/report.json")).unwrap();
    let result: Calibration =
        serde_json::from_slice(&fs::read(root.path().join("run/calibration.json")).unwrap())
            .unwrap();
    assert_eq!(result.report_sha256, protocol::sha256(&bytes));
    assert!(!result.qualified);
    assert!(result.reasons.contains(&Reason::BuildsUnavailable));
    assert!(result.reasons.contains(&Reason::RunIncomplete));
    assert!(!result.reasons.contains(&Reason::ReferenceRevisionMismatch));
}
