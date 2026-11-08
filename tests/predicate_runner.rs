#![cfg(unix)]

#[path = "../src/predicate/calibration.rs"]
mod calibration;
#[path = "../src/predicate/case.rs"]
mod case;
#[path = "../src/predicate/longhand.rs"]
mod longhand;
#[path = "../src/predicate/protocol.rs"]
mod protocol;
#[path = "../src/predicate/runner.rs"]
mod runner;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use case::{Case, Filter};
use protocol::{Context, Identity, Observed, Progress, Stage, WorkerRequest};
use runner::{IssueKind, WorkerObservation};
use serde_json::{json, Value};

struct Fixture {
    root: tempfile::TempDir,
    case: Case,
    request: WorkerRequest,
    identity: Identity,
    argv: Vec<String>,
    records: BTreeMap<String, Value>,
    raw: BTreeMap<String, String>,
    body: String,
    timeout: Duration,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let bytes = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("cases/intersects/plane_later_member_containment.json"),
        )
        .unwrap();
        let case: Case = serde_json::from_slice(&bytes).unwrap();
        let identity = Identity {
            protocol: 1,
            build_record_sha256: Some("a".repeat(64)),
            engine_revision: Some("b".repeat(40)),
        };
        fs::write(root_path.join("case.json"), &bytes).unwrap();
        let request = WorkerRequest {
            case: root_path.join("case.json").to_str().unwrap().into(),
            case_sha256: protocol::sha256(&bytes),
            build_record_sha256: identity.build_record_sha256.clone().unwrap(),
            engine_revision: identity.engine_revision.clone().unwrap(),
            output: root_path.join("worker").to_str().unwrap().into(),
        };
        let argv = vec![
            root_path.join("fixture").to_str().unwrap().into(),
            "worker".into(),
        ];
        let context = Context {
            requested: request.clone(),
            compiled: identity.clone(),
            observed: Observed {
                case_sha256: Some(request.case_sha256.clone()),
            },
            identity_verified: Some(true),
            argv: argv.clone(),
        };
        let header = context.verify().unwrap();
        let records = BTreeMap::from([
            (
                "stage.json".into(),
                serde_json::to_value(Progress {
                    context,
                    stage: Stage::Result,
                })
                .unwrap(),
            ),
            (
                "result.json".into(),
                json!({"header": header, "actual": ["point"]}),
            ),
            (
                "diagnostics.json".into(),
                json!({
                    "header": header, "segment_count": 1, "segment_id": "synthetic",
                    "max_doc": 2, "deleted_docs": 0,
                    "stored_ids": [{"doc_id": 0, "id": "point"}, {"doc_id": 1, "id": "decoy"}],
                    "filter": case.filter, "selected_ids": ["decoy", "point"],
                    "sidecar_requested": false, "sidecar_available": false, "sidecar_used": false,
                    "cell_count": 1,
                    "clipper": {"max_edges_per_cell": 10, "min_short_edge_fraction": 0.2},
                    "seeds": []
                }),
            ),
        ]);
        Self {
            root,
            case,
            request,
            identity,
            argv,
            records,
            raw: BTreeMap::new(),
            body: "exit 0\n".into(),
            timeout: Duration::from_secs(5),
        }
    }

    fn record(&mut self, name: &str) -> &mut Value {
        self.records.get_mut(name).unwrap()
    }

    fn run(self) -> WorkerObservation {
        let root = self.root.path().canonicalize().unwrap();
        let logs = root.join("logs");
        fs::create_dir(root.join("records")).unwrap();
        for (name, value) in self.records {
            if name == "stage.json" {
                let progress: Progress = serde_json::from_value(value).unwrap();
                protocol::progress(&root.join("records"), &progress.context, progress.stage)
                    .unwrap();
            } else {
                fs::write(
                    root.join("records").join(name),
                    serde_json::to_vec(&value).unwrap(),
                )
                .unwrap();
            }
        }
        for (name, text) in self.raw {
            fs::write(root.join("records").join(name), text).unwrap();
        }
        let script = format!(
            "#!/bin/sh\nset -eu\nmkdir worker\ncp records/* worker/\n{}",
            self.body
        );
        fs::write(&self.argv[0], script).unwrap();
        fs::set_permissions(&self.argv[0], fs::Permissions::from_mode(0o700)).unwrap();
        let process = runner::supervise(&self.argv, &root, &logs, self.timeout).unwrap();
        let persisted: runner::ProcessObservation =
            serde_json::from_slice(&fs::read(logs.join("process.json")).unwrap()).unwrap();
        assert_eq!(process, persisted);
        runner::observe_worker(
            &self.case,
            &self.case.expected.ids,
            &self.request,
            &self.identity,
            process,
            &logs,
        )
        .unwrap()
    }
}

fn has_issue(observation: &WorkerObservation, kind: IssueKind) -> bool {
    observation
        .protocol_issues
        .iter()
        .any(|issue| issue.kind == kind)
}

#[test]
fn test_success_and_both_sides_of_a_mismatch() {
    let fixture = Fixture::new();
    assert_eq!(
        longhand::evaluate(&fixture.case).unwrap().expected,
        ["point"]
    );
    let observation = fixture.run();
    assert!(observation.agreement);
    assert!(observation.protocol_issues.is_empty());
    assert_eq!(observation.semantic.unwrap().actual, ["point"]);

    let mut fixture = Fixture::new();
    fixture.case.filter = Filter::Exclude {
        ids: vec!["decoy".into()],
    };
    let filter = serde_json::to_value(&fixture.case.filter).unwrap();
    fixture.record("diagnostics.json")["filter"] = filter;
    fixture.record("diagnostics.json")["selected_ids"] = json!(["point"]);
    fixture.record("result.json")["actual"] = json!(["decoy"]);
    let observation = fixture.run();
    assert!(!observation.agreement);
    assert!(observation.protocol_issues.is_empty());
    let semantic = observation.semantic.unwrap();
    assert_eq!(semantic.missing, ["point"]);
    assert_eq!(semantic.spurious, ["decoy"]);
}

#[test]
fn test_result_payload_must_be_complete_canonical_and_known() {
    for (actual, kind) in [
        (json!(["point", "point"]), IssueKind::NoncanonicalIds),
        (json!(["point", "decoy"]), IssueKind::NoncanonicalIds),
        (json!(["unknown"]), IssueKind::UnknownId),
    ] {
        let mut fixture = Fixture::new();
        fixture.record("result.json")["actual"] = actual;
        let observation = fixture.run();
        assert!(has_issue(&observation, kind));
        assert!(observation.semantic.is_none());
        assert!(!observation.agreement);
    }
    for malformed in [
        "{",
        "{}",
        "{\"header\": null, \"actual\": []}",
        "[]",
        "null",
    ] {
        let mut fixture = Fixture::new();
        fixture.raw.insert("result.json".into(), malformed.into());
        let observation = fixture.run();
        assert!(has_issue(&observation, IssueKind::MalformedRecord));
        assert!(observation.semantic.is_none());
    }
    let mut fixture = Fixture::new();
    fixture.records.remove("result.json");
    let observation = fixture.run();
    assert!(has_issue(&observation, IssueKind::MissingRecord));
    assert!(observation.semantic.is_none());
}

#[test]
fn test_stale_result_headers_and_progress_contexts_are_not_compared() {
    for key in ["case_sha256", "build_record_sha256", "engine_revision"] {
        let mut fixture = Fixture::new();
        fixture.record("result.json")["header"][key] = json!("c".repeat(64));
        let observation = fixture.run();
        assert!(has_issue(&observation, IssueKind::HeaderMismatch));
        assert!(observation.semantic.is_none());
    }
    for key in [
        "requested",
        "compiled",
        "observed",
        "argv",
        "identity_verified",
    ] {
        let mut fixture = Fixture::new();
        let context = &mut fixture.record("stage.json")["context"];
        match key {
            "requested" => context[key]["output"] = json!("elsewhere"),
            "compiled" => context[key]["engine_revision"] = json!("c".repeat(40)),
            "observed" => context[key]["case_sha256"] = json!("c".repeat(64)),
            "argv" => context[key] = json!(["another-worker"]),
            _ => context[key] = json!(false),
        }
        let observation = fixture.run();
        assert!(has_issue(&observation, IssueKind::ContextMismatch), "{key}");
        assert!(observation.progress.is_none());
        assert!(observation.semantic.is_none());
    }
}

#[test]
fn test_a_result_file_does_not_establish_completion() {
    for (stage, body) in [
        ("query", "exit 0\n"),
        ("result", "exit 1\n"),
        ("result", "exit 101\n"),
    ] {
        let mut fixture = Fixture::new();
        fixture.record("stage.json")["stage"] = json!(stage);
        fixture.body = body.into();
        let observation = fixture.run();
        assert!(has_issue(&observation, IssueKind::ResultWithoutCompletion));
        assert!(observation.semantic.is_none());
        assert!(observation.panic.is_none());
    }
}

#[test]
fn test_bad_diagnostics_retain_an_independent_semantic_mismatch() {
    for fault in [
        "missing",
        "malformed",
        "header",
        "ids",
        "doc_ids",
        "filter",
        "selected",
        "sidecar",
    ] {
        let mut fixture = Fixture::new();
        fixture.record("result.json")["actual"] = json!(["decoy"]);
        match fault {
            "missing" => {
                fixture.records.remove("diagnostics.json");
            }
            "malformed" => {
                fixture.raw.insert("diagnostics.json".into(), "{".into());
            }
            "header" => {
                fixture.record("diagnostics.json")["header"]["case_sha256"] = json!("c".repeat(64))
            }
            "ids" => fixture.record("diagnostics.json")["stored_ids"][0]["id"] = json!("another"),
            "doc_ids" => fixture.record("diagnostics.json")["stored_ids"][1]["doc_id"] = json!(0),
            "filter" => fixture.record("diagnostics.json")["filter"] = json!({"mode": "all"}),
            "selected" => fixture.record("diagnostics.json")["selected_ids"] = json!([]),
            _ => fixture.record("diagnostics.json")["sidecar_requested"] = json!(true),
        }
        let observation = fixture.run();
        assert!(
            has_issue(&observation, IssueKind::DiagnosticsRejected),
            "{fault}"
        );
        assert!(observation.diagnostics.facts.is_none());
        assert!(!observation.diagnostics.issues.is_empty());
        assert!(!observation.agreement);
        let semantic = observation.semantic.unwrap();
        assert_eq!(semantic.missing, ["point"]);
        assert_eq!(semantic.spurious, ["decoy"]);
    }
}

#[test]
fn test_setup_error_is_an_observation_without_a_missing_result_complaint() {
    for shape in ["unread", "refused", "verified", "index", "diagnostics"] {
        let stage = match shape {
            "unread" | "refused" | "verified" => "validate",
            _ => shape,
        };
        let mut fixture = Fixture::new();
        fixture.records.remove("result.json");
        fixture.records.remove("diagnostics.json");
        fixture.record("stage.json")["stage"] = json!(stage);
        if shape == "unread" {
            fixture.record("stage.json")["context"]["observed"]["case_sha256"] = Value::Null;
            fixture.record("stage.json")["context"]["identity_verified"] = Value::Null;
        }
        if shape == "refused" {
            fixture.record("stage.json")["context"]["observed"]["case_sha256"] =
                json!("c".repeat(64));
            fixture.record("stage.json")["context"]["identity_verified"] = json!(false);
        }
        let mut error = fixture.record("stage.json").clone();
        error["message"] = json!("synthetic setup error");
        fixture.records.insert("error.json".into(), error);
        fixture.body = "exit 1\n".into();
        let observation = fixture.run();
        assert!(observation.protocol_issues.is_empty());
        assert!(observation.diagnostics.issues.is_empty());
        assert_eq!(observation.error.unwrap().message, "synthetic setup error");
        assert!(observation.semantic.is_none());
    }
}

#[test]
fn test_panic_requires_both_exit_and_standard_stderr_evidence() {
    for (thread, number) in [("main", ""), ("main", " (1234)"), ("other", " (4321)")] {
        let mut fixture = Fixture::new();
        fixture.records.remove("result.json");
        fixture.record("stage.json")["stage"] = json!("query");
        fixture.body = format!("cat >&2 <<'TEXT'\nthread '{thread}'{number} panicked at source.rs:10:20:\nindex out of bounds: the len is 4 but the index is 4\nTEXT\nexit 101\n");
        let observation = fixture.run();
        assert!(observation.protocol_issues.is_empty());
        assert_eq!(observation.progress.unwrap().stage, Stage::Query);
        assert!(observation.diagnostics.facts.is_some());
        let panic = observation.panic.unwrap();
        assert_eq!(panic.thread, thread);
        assert_eq!(panic.location, "source.rs:10:20");
        assert!(panic.message.contains("index out of bounds"));
    }
    let mut fixture = Fixture::new();
    fixture.records.remove("result.json");
    fixture.body = "echo 'panicked at source.rs' >&2\nexit 101\n".into();
    assert!(fixture.run().panic.is_none());
}

#[test]
fn test_signal_and_timeout_are_reaped_without_admitting_partial_results() {
    let mut fixture = Fixture::new();
    fixture.body = "kill -TERM $$\n".into();
    let observation = fixture.run();
    assert_eq!(observation.process.waited.unwrap().signal, Some(15));
    assert!(!observation.process.timed_out);
    assert!(observation.semantic.is_none());

    let mut fixture = Fixture::new();
    fixture.body = "exec /bin/sleep 30\n".into();
    fixture.timeout = Duration::from_millis(200);
    let observation = fixture.run();
    assert!(observation.process.timed_out);
    assert!(observation.process.kill_requested);
    assert!(observation.process.kill_error.is_none());
    assert_eq!(observation.process.waited.unwrap().signal, Some(9));
    assert!(observation.process.orchestration_error.is_none());
    assert!(observation.semantic.is_none());
    assert!(!observation.agreement);
}

#[test]
fn test_stdout_and_contradictory_error_records_deny_agreement() {
    let mut fixture = Fixture::new();
    fixture.body = "echo unexpected\nexit 0\n".into();
    let observation = fixture.run();
    assert!(has_issue(&observation, IssueKind::UnexpectedStdout));
    assert_eq!(observation.stdout_excerpt.as_deref(), Some("unexpected\n"));
    assert!(observation.semantic.is_some());
    assert!(!observation.agreement);

    let mut fixture = Fixture::new();
    let mut error = fixture.record("stage.json").clone();
    error["message"] = json!("contradictory error");
    fixture.records.insert("error.json".into(), error);
    let observation = fixture.run();
    assert!(has_issue(&observation, IssueKind::UnexpectedError));
    assert!(observation.semantic.is_none());
}

#[test]
fn test_supervisor_refuses_log_reuse_before_launch() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let logs = path.join("logs");
    let argv = vec!["/bin/sh".into(), "-c".into(), "exit 0".into()];
    let process = runner::supervise(&argv, &path, &logs, Duration::from_secs(5)).unwrap();
    assert!(process.success());
    let before = fs::read(logs.join("process.json")).unwrap();
    assert!(runner::supervise(&argv, &path, &logs, Duration::from_secs(5)).is_err());
    assert_eq!(fs::read(logs.join("process.json")).unwrap(), before);
}

fn worker_entry(root: &Path, name: &str, revision: char) -> runner::WorkerEntry {
    let revision = revision.to_string().repeat(40);
    let record =
        serde_json::to_vec(&json!({"version": 1, "engine_revision": revision, "synthetic": true}))
            .unwrap();
    let digest = protocol::sha256(&record);
    fs::write(root.join(format!("{name}.json")), record).unwrap();
    let identity = Identity {
        protocol: 1,
        build_record_sha256: Some(digest.clone()),
        engine_revision: Some(revision.clone()),
    };
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = identity ] || exit 1\nprintf '%s' '{}'\n",
        serde_json::to_string(&identity).unwrap()
    );
    let path = root.join(name);
    fs::write(&path, &script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    runner::WorkerEntry {
        executable: name.into(),
        executable_sha256: protocol::sha256(script.as_bytes()),
        build_record: format!("{name}.json"),
        build_record_sha256: digest,
        engine_revision: revision,
    }
}

fn worker_manifest(root: &Path) -> runner::WorkersManifest {
    let baseline = worker_entry(root, "baseline", 'b');
    let repaired = worker_entry(root, "repaired", 'c');
    runner::WorkersManifest {
        version: 1,
        reference: repaired.clone(),
        baseline,
        repaired,
    }
}

fn reference_identity(manifest: &runner::WorkersManifest) -> runner::ReferenceIdentity {
    runner::ReferenceIdentity {
        executable: manifest.reference.executable.clone(),
        executable_sha256: manifest.reference.executable_sha256.clone(),
        compiled: Identity {
            protocol: 1,
            build_record_sha256: Some(manifest.reference.build_record_sha256.clone()),
            engine_revision: Some(manifest.reference.engine_revision.clone()),
        },
    }
}

#[test]
fn test_preflight_verifies_explicit_copies_and_shared_reference() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = worker_manifest(&root);
    let copies = root.join("copies");
    let workers = runner::preflight(
        &manifest,
        &root,
        &copies,
        &reference_identity(&manifest),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(workers.reference.executable, workers.repaired.executable);
    assert_ne!(workers.baseline.executable, workers.repaired.executable);
    assert_eq!(
        protocol::sha256(&fs::read(&workers.baseline.executable).unwrap()),
        manifest.baseline.executable_sha256
    );
    assert_eq!(
        protocol::sha256(&fs::read(&workers.repaired.build_record).unwrap()),
        manifest.repaired.build_record_sha256
    );
    assert!(workers.reference.identity_process.success());
    assert!(workers.baseline.identity_process.success());
    assert!(workers.repaired.identity_process.success());
    assert!(Path::new(&workers.repaired.executable).starts_with(copies));
}

#[test]
fn test_preflight_refuses_aliases_and_parent_identity_mismatches() {
    for fault in [
        "same_binary",
        "same_engine",
        "parent_hash",
        "parent_stamp",
        "unstamped",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut manifest = worker_manifest(&root);
        let mut reference = reference_identity(&manifest);
        match fault {
            "same_binary" => {
                manifest.baseline.executable_sha256 = manifest.repaired.executable_sha256.clone()
            }
            "same_engine" => {
                manifest.baseline.engine_revision = manifest.repaired.engine_revision.clone()
            }
            "parent_hash" => reference.executable_sha256 = "d".repeat(64),
            "parent_stamp" => reference.compiled.engine_revision = Some("d".repeat(40)),
            _ => reference.compiled.build_record_sha256 = None,
        }
        let copies = root.join("copies");
        assert!(
            runner::preflight(
                &manifest,
                &root,
                &copies,
                &reference,
                Duration::from_secs(5)
            )
            .is_err(),
            "{fault}"
        );
        assert!(!copies.exists());
    }
}

#[test]
fn test_preflight_refuses_tampered_records_executables_and_identity_responses() {
    for fault in [
        "record_hash",
        "record_engine",
        "binary_hash",
        "compiled",
        "missing_reused_path",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut manifest = worker_manifest(&root);
        match fault {
            "record_hash" => fs::write(root.join("baseline.json"), b"{}").unwrap(),
            "record_engine" => {
                let bytes =
                    serde_json::to_vec(&json!({"version": 1, "engine_revision": "d".repeat(40)}))
                        .unwrap();
                fs::write(root.join("baseline.json"), &bytes).unwrap();
                manifest.baseline.build_record_sha256 = protocol::sha256(&bytes);
            }
            "binary_hash" => fs::write(root.join("baseline"), b"modified").unwrap(),
            "compiled" => {
                let path = root.join("baseline");
                let script = fs::read_to_string(&path)
                    .unwrap()
                    .replace(&manifest.baseline.engine_revision, &"d".repeat(40));
                fs::write(path, &script).unwrap();
                manifest.baseline.executable_sha256 = protocol::sha256(script.as_bytes());
            }
            _ => manifest.repaired.executable = "absent-alias".into(),
        }
        let error = runner::preflight(
            &manifest,
            &root,
            &root.join("copies"),
            &reference_identity(&manifest),
            Duration::from_secs(5),
        )
        .unwrap_err()
        .to_string();
        let expected = match fault {
            "record_hash" => "baseline build record digest",
            "record_engine" => "baseline build record version or engine revision",
            "binary_hash" => "baseline executable digest",
            "compiled" => "baseline compiled identity",
            _ => "No such file or directory",
        };
        assert!(error.contains(expected), "{fault}: {error}");
    }
}

#[test]
fn test_manifest_shape_and_corpus_paths_are_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = worker_manifest(temp.path());
    let mut value = serde_json::to_value(&manifest).unwrap();
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<runner::WorkersManifest>(value).is_err());
    let mut value = serde_json::to_value(manifest).unwrap();
    value.as_object_mut().unwrap().remove("reference");
    assert!(serde_json::from_value::<runner::WorkersManifest>(value).is_err());

    let input = Path::new("/recorded/corpus.json");
    let (paths, _) = runner::replay_input(
        input,
        br#"{"version":1,"cases":["sphere/a.json","plane/b.json"]}"#,
    )
    .unwrap();
    assert_eq!(
        paths,
        [
            Path::new("/recorded/sphere/a.json"),
            Path::new("/recorded/plane/b.json")
        ]
    );
    for bytes in [
        br#"{"version":1,"cases":[]}"#.as_slice(),
        br#"{"version":1,"cases":["/absolute.json"]}"#.as_slice(),
        br#"{"version":1,"cases":[""]}"#.as_slice(),
        br#"{"version":2,"cases":["case.json"]}"#.as_slice(),
        br#"{"version":1,"cases":["case.json"],"extra":true}"#.as_slice(),
    ] {
        assert!(runner::replay_input(input, bytes).is_err());
    }
    assert_eq!(runner::replay_input(input, b"{").unwrap().0, [input]);
}

#[test]
fn test_reference_outcomes_distinguish_expectation_rejection_and_failure() {
    let fixture = Fixture::new();
    let mut case = fixture.case;
    assert!(matches!(
        runner::evaluate_reference(&case, longhand::evaluate),
        runner::ReferenceOutcome::Answered { .. }
    ));
    case.expected.ids.clear();
    assert!(matches!(
        runner::evaluate_reference(&case, longhand::evaluate),
        runner::ReferenceOutcome::ExpectationMismatch { .. }
    ));
    case.margin = 0.0;
    assert!(matches!(
        runner::evaluate_reference(&case, longhand::evaluate),
        runner::ReferenceOutcome::Rejected { .. }
    ));
    fn broken_reference(_: &Case) -> Result<longhand::Answer, case::Rejection> {
        panic!("synthetic reference fault")
    }
    assert_eq!(
        runner::evaluate_reference(&case, broken_reference),
        runner::ReferenceOutcome::Failed {
            message: "synthetic reference fault".into()
        }
    );
}

#[test]
fn test_replay_preserves_a_preflight_failure_and_refuses_output_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let manifest = worker_manifest(root);
    fs::write(
        root.join("workers.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let case = Fixture::new().case;
    let bytes = serde_json::to_vec(&case).unwrap();
    fs::write(root.join("case.json"), &bytes).unwrap();
    let request = runner::ReplayRequest {
        input: root.join("case.json"),
        workers: root.join("workers.json"),
        output: root.join("run"),
        timeout_seconds: 1,
    };
    assert_eq!(
        runner::replay(request.clone(), vec!["synthetic-replay-test".into()]).unwrap(),
        1
    );
    assert_eq!(fs::read(root.join("run/input.json")).unwrap(), bytes);
    assert!(!root.join("run/cases").exists());
    let before = fs::read(root.join("run/report.json")).unwrap();
    let mut report: runner::RunReport = serde_json::from_slice(&before).unwrap();
    assert!(!report.complete);
    assert!(report.error.is_some());
    assert_eq!(report.exit_code(), 1);
    report.complete = true;
    report.error = None;
    report.reference_failed = true;
    assert_eq!(report.exit_code(), 2);
    report.reference_failed = false;
    assert_eq!(report.exit_code(), 0);
    assert!(runner::replay(request, vec![]).is_err());
    assert_eq!(fs::read(root.join("run/report.json")).unwrap(), before);
}

#[test]
fn test_replay_continues_after_input_and_reference_failures() {
    fn reference(case: &Case) -> Result<longhand::Answer, case::Rejection> {
        if case.name == "reference_fault" {
            panic!("synthetic reference fault")
        }
        longhand::evaluate(case)
    }
    for fail_reference in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let manifest = worker_manifest(&root);
        fs::write(
            root.join("workers.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let base = Fixture::new().case;
        fs::write(root.join("malformed.json"), b"{").unwrap();
        let mut rejected = base.clone();
        rejected.margin = 0.0;
        fs::write(
            root.join("rejected.json"),
            serde_json::to_vec(&rejected).unwrap(),
        )
        .unwrap();
        let mut mismatch = base.clone();
        mismatch.expected.ids.clear();
        fs::write(
            root.join("mismatch.json"),
            serde_json::to_vec(&mismatch).unwrap(),
        )
        .unwrap();
        let mut fault = base.clone();
        fault.name = "reference_fault".into();
        fs::write(root.join("fault.json"), serde_json::to_vec(&fault).unwrap()).unwrap();
        fs::write(
            root.join("answered.json"),
            serde_json::to_vec(&base).unwrap(),
        )
        .unwrap();
        let mut paths = vec![
            "unreadable.json",
            "malformed.json",
            "rejected.json",
            "mismatch.json",
        ];
        if fail_reference {
            paths.push("fault.json");
        }
        paths.push("answered.json");
        fs::write(
            root.join("corpus.json"),
            serde_json::to_vec(&json!({"version": 1, "cases": paths})).unwrap(),
        )
        .unwrap();
        let output = root.join("run");
        let request = runner::ReplayRequest {
            input: root.join("corpus.json"),
            workers: root.join("workers.json"),
            output: output.clone(),
            timeout_seconds: 5,
        };
        let code = runner::replay_with_reference(
            request,
            vec!["synthetic-replay-test".into()],
            || Ok(reference_identity(&manifest)),
            reference,
        )
        .unwrap();
        assert_eq!(code, if fail_reference { 2 } else { 0 });
        let report: runner::RunReport =
            serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
        assert!(report.complete);
        assert!(report.error.is_none());
        assert_eq!(report.reference_failed, fail_reference);
        assert_eq!(
            report.canonical_input,
            Some(root.join("corpus.json").to_str().unwrap().into())
        );
        assert_eq!(
            report.canonical_manifest,
            Some(root.join("workers.json").to_str().unwrap().into())
        );
        assert_eq!(report.cases.len(), paths.len());
        assert!(matches!(
            report.cases[0].reference,
            runner::ReferenceOutcome::Unreadable { .. }
        ));
        assert!(matches!(
            report.cases[1].reference,
            runner::ReferenceOutcome::Malformed { .. }
        ));
        assert!(matches!(
            report.cases[2].reference,
            runner::ReferenceOutcome::Rejected { .. }
        ));
        assert!(matches!(
            report.cases[3].reference,
            runner::ReferenceOutcome::ExpectationMismatch { .. }
        ));
        if fail_reference {
            assert!(matches!(
                report.cases[4].reference,
                runner::ReferenceOutcome::Failed { .. }
            ));
        }
        for (number, case) in report.cases.iter().enumerate() {
            if number == paths.len() - 1 {
                assert!(matches!(
                    case.reference,
                    runner::ReferenceOutcome::Answered { .. }
                ));
                assert!(case.baseline.is_some() && case.repaired.is_some());
            } else {
                assert!(case.baseline.is_none() && case.repaired.is_none());
            }
            let directory = output.join("cases").join(format!("{number:04}"));
            assert!(directory.is_dir());
            if number == 0 {
                assert!(case.preserved.is_none());
            } else {
                assert_eq!(
                    fs::read(directory.join("case.json")).unwrap(),
                    fs::read(root.join(paths[number])).unwrap()
                );
            }
        }
    }
}
