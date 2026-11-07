#[path = "../src/predicate/case.rs"]
mod case;
#[path = "../src/predicate/engine.rs"]
mod engine;
#[path = "../src/predicate/protocol.rs"]
mod protocol;

use std::fs;
use std::path::Path;

use case::{Case, Filter, Geometry};
use protocol::{
    Completed, Context, Diagnostics, Identity, Observed, Progress, Stage, WorkerError,
    WorkerRequest, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;
use tantivy::schema::Value;
use tantivy::{DocAddress, TantivyDocument};

fn identity() -> Identity {
    Identity {
        protocol: PROTOCOL_VERSION,
        build_record_sha256: Some("a".repeat(64)),
        engine_revision: Some("b".repeat(40)),
    }
}

fn fixture(surface: &str) -> Case {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "cases/intersects/{surface}_later_member_containment.json"
    ));
    let mut case: Case = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let Geometry::MultiPolygon { mut coordinates } = case.query else {
        panic!()
    };
    case.query = Geometry::Polygon {
        coordinates: coordinates.remove(1),
    };
    case
}

fn request(root: &Path, case: &Case) -> WorkerRequest {
    let path = root.join("case.json");
    let bytes = serde_json::to_vec(case).unwrap();
    fs::write(&path, &bytes).unwrap();
    WorkerRequest {
        case: path.to_str().unwrap().into(),
        case_sha256: protocol::sha256(&bytes),
        build_record_sha256: "a".repeat(64),
        engine_revision: "b".repeat(40),
        output: root.join("worker").to_str().unwrap().into(),
    }
}

fn read<T: DeserializeOwned>(root: &Path, name: &str) -> T {
    serde_json::from_slice(&fs::read(root.join("worker").join(name)).unwrap()).unwrap()
}

#[test]
fn test_worker_ingests_and_searches_both_surfaces_and_filters() {
    for surface in ["plane", "sphere"] {
        for (filter, hit) in [
            (Filter::None {}, true),
            (Filter::All {}, true),
            (
                Filter::Include {
                    ids: vec!["point".into()],
                },
                true,
            ),
            (Filter::Include { ids: vec![] }, false),
            (
                Filter::Exclude {
                    ids: vec!["point".into()],
                },
                false,
            ),
            (
                Filter::Exclude {
                    ids: vec!["decoy".into()],
                },
                true,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut case = fixture(surface);
            case.filter = filter;
            case.sidecar = false;
            case.expected.ids = if hit { vec!["point".into()] } else { vec![] };
            let req = request(root.path(), &case);
            engine::run(
                req.clone(),
                vec!["synthetic-worker-test".into()],
                identity(),
            )
            .unwrap();
            let result: Completed = read(root.path(), "result.json");
            assert_eq!(result.actual, case.expected.ids);
            assert_eq!(result.header.case_sha256, req.case_sha256);
            assert_eq!(result.header.engine_revision, req.engine_revision);
            let progress: Progress = read(root.path(), "stage.json");
            assert_eq!(progress.stage, Stage::Result);
            assert_eq!(progress.context.identity_verified, Some(true));
            assert_eq!(
                progress.context.observed.case_sha256.as_deref(),
                Some(req.case_sha256.as_str())
            );
            assert_eq!(progress.context.requested, req);
            assert_eq!(progress.context.argv, ["synthetic-worker-test"]);
            let diagnostics: Diagnostics = read(root.path(), "diagnostics.json");
            assert_eq!(diagnostics.header, result.header);
            assert_eq!(diagnostics.segment_count, 1);
            assert_eq!(diagnostics.max_doc, 2);
            assert_eq!(diagnostics.deleted_docs, 0);
            assert_eq!(
                diagnostics
                    .stored_ids
                    .iter()
                    .map(|id| id.id.as_str())
                    .collect::<Vec<_>>(),
                ["point", "decoy"]
            );
            assert_eq!(diagnostics.filter, case.filter);
            let mut selected: Vec<_> = case
                .documents
                .iter()
                .filter(|doc| case.filter.selected(&doc.id))
                .map(|doc| doc.id.clone())
                .collect();
            selected.sort();
            assert_eq!(diagnostics.selected_ids, selected);
            assert!(!diagnostics.sidecar_requested);
            assert!(diagnostics.sidecar_available);
            assert!(!diagnostics.sidecar_used);
            assert!(!diagnostics.seeds.is_empty());
            for seed in &diagnostics.seeds {
                assert_eq!(seed.member, 0);
                assert_eq!(seed.member_vertices, 5);
                assert_eq!(seed.member_zero_vertices, 5);
                assert!(seed
                    .edge_indices
                    .iter()
                    .all(|&edge| (edge as usize + 1) < seed.member_vertices));
                assert!(seed.index_start <= seed.index_end);
                assert!(seed.index_end <= diagnostics.cell_count);
                assert_eq!(
                    seed.first_index_level.is_some(),
                    seed.index_start < seed.index_end
                );
            }
            assert_eq!(diagnostics.clipper.max_edges_per_cell, 10);
            assert_eq!(diagnostics.clipper.min_short_edge_fraction, 0.2);
            assert!(!root.path().join("worker/error.json").exists());
        }
    }
}

#[test]
fn test_requested_sidecar_is_used() {
    for surface in ["plane", "sphere"] {
        let root = tempfile::tempdir().unwrap();
        let mut case = fixture(surface);
        case.sidecar = true;
        let req = request(root.path(), &case);
        engine::run(req, Vec::new(), identity()).unwrap();
        let result: Completed = read(root.path(), "result.json");
        assert_eq!(result.actual, ["point"]);
        let diagnostics: Diagnostics = read(root.path(), "diagnostics.json");
        assert!(diagnostics.sidecar_requested);
        assert!(diagnostics.sidecar_available);
        assert!(diagnostics.sidecar_used);
        let progress: Progress = read(root.path(), "stage.json");
        assert_eq!(progress.stage, Stage::Result);
        assert!(!root.path().join("worker/error.json").exists());
    }
}

#[test]
fn test_stored_ids_are_recovered_independently_of_expected_order() {
    let root = tempfile::tempdir().unwrap();
    let case = fixture("plane");
    let index = engine::build(&case, &root.path().join("index")).unwrap();
    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    let field = index.schema().get_field("id").unwrap();
    let ids = engine::recover_ids(&searcher, field, &["decoy", "point"]).unwrap();
    assert_eq!(
        ids.iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["point", "decoy"]
    );
    for entry in &ids {
        let stored: TantivyDocument = searcher.doc(DocAddress::new(0, entry.doc_id)).unwrap();
        assert_eq!(
            stored.get_first(field).unwrap().as_str(),
            Some(entry.id.as_str())
        );
    }
    for expected in [
        vec!["point"],
        vec!["point", "point"],
        vec!["point", "substitute"],
    ] {
        assert!(engine::recover_ids(&searcher, field, &expected).is_err());
    }
}

#[test]
fn test_duplicate_stored_id_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut case = fixture("plane");
    case.documents[1].id = "point".into();
    let index = engine::build(&case, &root.path().join("index")).unwrap();
    let reader = index.reader().unwrap();
    assert!(engine::recover_ids(
        &reader.searcher(),
        index.schema().get_field("id").unwrap(),
        &["point", "decoy"]
    )
    .is_err());
}

#[test]
fn test_worker_identity_mismatch_never_indexes() {
    for mismatch in 0..5 {
        let root = tempfile::tempdir().unwrap();
        let mut req = request(root.path(), &fixture("plane"));
        let mut compiled = identity();
        match mismatch {
            0 => compiled.build_record_sha256 = None,
            1 => req.build_record_sha256 = "c".repeat(64),
            2 => req.engine_revision = "c".repeat(40),
            3 => req.case_sha256 = "c".repeat(64),
            4 => compiled.protocol += 1,
            _ => unreachable!(),
        }
        assert!(engine::run(
            req.clone(),
            vec!["synthetic-worker-test".into()],
            compiled.clone()
        )
        .is_err());
        let error: WorkerError = read(root.path(), "error.json");
        assert_eq!(error.stage, Stage::Validate);
        assert_eq!(error.context.compiled, compiled);
        assert_eq!(error.context.requested, req);
        assert!(error.context.observed.case_sha256.is_some());
        assert_eq!(error.context.identity_verified, Some(false));
        assert!(!root.path().join("worker/index").exists());
        assert!(!root.path().join("worker/result.json").exists());
    }
}

#[test]
fn test_unread_input_has_no_observed_digest() {
    let root = tempfile::tempdir().unwrap();
    let req = request(root.path(), &fixture("plane"));
    fs::remove_file(&req.case).unwrap();
    assert!(engine::run(req, Vec::new(), identity()).is_err());
    let error: WorkerError = read(root.path(), "error.json");
    assert_eq!(error.stage, Stage::Validate);
    assert_eq!(error.context.observed.case_sha256, None);
    assert_eq!(error.context.identity_verified, None);
    let progress: Progress = read(root.path(), "stage.json");
    assert_eq!(progress.context, error.context);
    assert!(!root.path().join("worker/index").exists());
}

#[test]
fn test_bad_input_is_rejected_before_index_creation() {
    for malformed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut case = fixture("plane");
        case.query = Geometry::LineString {
            coordinates: vec![[0., 0.], [1., 1.]],
        };
        let mut req = request(root.path(), &case);
        if malformed {
            fs::write(&req.case, b"{").unwrap();
            req.case_sha256 = protocol::sha256(b"{");
        }
        assert!(engine::run(req, Vec::new(), identity()).is_err());
        let error: WorkerError = read(root.path(), "error.json");
        assert_eq!(error.stage, Stage::Validate);
        assert_eq!(error.context.identity_verified, Some(true));
        assert!(!root.path().join("worker/index").exists());
    }
}

#[test]
fn test_existing_output_is_not_reused() {
    let root = tempfile::tempdir().unwrap();
    let req = request(root.path(), &fixture("plane"));
    fs::create_dir(&req.output).unwrap();
    let sentinel = Path::new(&req.output).join("sentinel");
    fs::write(&sentinel, b"keep").unwrap();
    assert!(engine::run(req, Vec::new(), identity()).is_err());
    assert_eq!(fs::read(sentinel).unwrap(), b"keep");
    assert_eq!(fs::read_dir(root.path().join("worker")).unwrap().count(), 1);
}

#[test]
fn test_identity_stamps_and_observed_values() {
    assert_eq!(Identity::compiled().protocol, PROTOCOL_VERSION);
    for bad in ["", "short", &"F".repeat(64)] {
        let mut compiled = identity();
        compiled.build_record_sha256 = Some(bad.into());
        assert!(compiled.validate().is_err());
    }
    let root = tempfile::tempdir().unwrap();
    let req = request(root.path(), &fixture("plane"));
    let mut context = Context {
        requested: req.clone(),
        compiled: identity(),
        observed: Observed { case_sha256: None },
        identity_verified: None,
        argv: Vec::new(),
    };
    assert!(context.verify().is_err());
    context.observed.case_sha256 = Some(req.case_sha256.clone());
    let header = context.verify().unwrap();
    assert_eq!(header.case_sha256, req.case_sha256);
}

#[test]
fn test_atomic_record_replacement_retains_previous_record_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("stage.json");
    protocol::write_json(&path, &Stage::Validate).unwrap();
    protocol::write_json(&path, &Stage::Index).unwrap();
    assert_eq!(
        serde_json::from_slice::<Stage>(&fs::read(&path).unwrap()).unwrap(),
        Stage::Index
    );
    fs::write(root.path().join("stage.json.tmp"), b"partial").unwrap();
    assert!(protocol::write_json(&path, &Stage::Query).is_err());
    assert_eq!(
        serde_json::from_slice::<Stage>(&fs::read(&path).unwrap()).unwrap(),
        Stage::Index
    );
}
