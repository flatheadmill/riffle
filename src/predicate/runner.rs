use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::case::{Case, Rejection};
use super::protocol::{
    self, Completed, Context, Diagnostics, Header, Identity, Progress, Result, Stage, WorkerError,
    WorkerRequest,
};
use super::{calibration, longhand};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl From<ExitStatus> for WaitStatus {
    fn from(status: ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: status.code(),
            signal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessObservation {
    pub invocation: Invocation,
    pub elapsed_ms: u64,
    pub timed_out: bool,
    pub kill_requested: bool,
    pub kill_error: Option<String>,
    pub waited: Option<WaitStatus>,
    pub orchestration_error: Option<String>,
}

impl ProcessObservation {
    pub fn success(&self) -> bool {
        !self.timed_out
            && self.orchestration_error.is_none()
            && self
                .waited
                .as_ref()
                .is_some_and(|status| status.code == Some(0))
    }
}

struct ChildGuard {
    child: Child,
    reaped: bool,
}

// Kill reaches this child only. Predicate workers do not spawn other processes.
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn supervise(
    argv: &[String],
    cwd: &Path,
    logs: &Path,
    timeout: Duration,
) -> Result<ProcessObservation> {
    if argv.is_empty() || !Path::new(&argv[0]).is_absolute() || !cwd.is_absolute() {
        return Err("child executable and working directory must be absolute".into());
    }
    let timeout_ms = u64::try_from(timeout.as_millis())?;
    if timeout_ms == 0 {
        return Err("child timeout must be at least one millisecond".into());
    }
    let invocation = Invocation {
        argv: argv.to_vec(),
        cwd: cwd.to_str().ok_or("working directory is not UTF-8")?.into(),
        environment: "inherited".into(),
        timeout_ms,
    };
    fs::create_dir(logs)?;
    protocol::write_json(&logs.join("invocation.json"), &invocation)?;
    let stdout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(logs.join("stdout"))?;
    let stderr = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(logs.join("stderr"))?;
    let start = Instant::now();
    let child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()?;
    let mut guard = ChildGuard {
        child,
        reaped: false,
    };
    let mut observation = ProcessObservation {
        invocation,
        elapsed_ms: 0,
        timed_out: false,
        kill_requested: false,
        kill_error: None,
        waited: None,
        orchestration_error: None,
    };
    loop {
        match guard.child.try_wait() {
            Ok(Some(status)) => {
                guard.reaped = true;
                observation.waited = Some(status.into());
                break;
            }
            Ok(None) => {}
            Err(error) => {
                observation.orchestration_error = Some(error.to_string());
                break;
            }
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            observation.timed_out = true;
            break;
        }
        thread::sleep(Duration::from_millis(10).min(timeout - elapsed));
    }
    if !guard.reaped {
        observation.kill_requested = true;
        observation.kill_error = guard.child.kill().err().map(|error| error.to_string());
        match guard.child.wait() {
            Ok(status) => {
                guard.reaped = true;
                observation.waited = Some(status.into());
            }
            Err(error) => {
                observation.orchestration_error = Some(format!("cannot reap child: {error}"));
            }
        }
    }
    observation.elapsed_ms = u64::try_from(start.elapsed().as_millis())?;
    protocol::write_json(&logs.join("process.json"), &observation)?;
    Ok(observation)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    MissingRecord,
    UnreadableRecord,
    MalformedRecord,
    ContextMismatch,
    HeaderMismatch,
    NoncanonicalIds,
    UnknownId,
    ResultWithoutCompletion,
    UnexpectedError,
    UnexpectedStdout,
    InvalidDiagnostics,
    DiagnosticsRejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub kind: IssueKind,
    pub record: String,
    pub message: String,
}

fn issue(issues: &mut Vec<Issue>, kind: IssueKind, record: &str, message: &str) {
    issues.push(Issue {
        kind,
        record: record.into(),
        message: message.into(),
    });
}

struct Record<T> {
    present: bool,
    value: Option<T>,
}

fn read_record<T: DeserializeOwned>(
    root: &Path,
    name: &str,
    required: bool,
    issues: &mut Vec<Issue>,
) -> Record<T> {
    let bytes = match fs::read(root.join(name)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if required {
                issue(issues, IssueKind::MissingRecord, name, "record is absent");
            }
            return Record {
                present: false,
                value: None,
            };
        }
        Err(error) => {
            issue(
                issues,
                IssueKind::UnreadableRecord,
                name,
                &error.to_string(),
            );
            return Record {
                present: true,
                value: None,
            };
        }
    };
    let value = match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(error) => {
            issue(issues, IssueKind::MalformedRecord, name, &error.to_string());
            None
        }
    };
    Record {
        present: true,
        value,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticComparison {
    pub actual: Vec<String>,
    pub missing: Vec<String>,
    pub spurious: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticObservation {
    pub facts: Option<Diagnostics>,
    pub issues: Vec<Issue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanicEvidence {
    pub thread: String,
    pub location: String,
    pub message: String,
}

pub fn panic_evidence(process: &ProcessObservation, stderr: &str) -> Option<PanicEvidence> {
    if process.timed_out || process.waited.as_ref()?.code != Some(101) {
        return None;
    }
    let mut lines = stderr.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("thread '") else {
            continue;
        };
        let Some((name, mut rest)) = rest.split_once("' ") else {
            continue;
        };
        if rest.starts_with('(') {
            let Some((number, after)) = rest[1..].split_once(") ") else {
                continue;
            };
            if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            rest = after;
        }
        if let Some(location) = rest.strip_prefix("panicked at ") {
            return Some(PanicEvidence {
                thread: name.into(),
                location: location.strip_suffix(':').unwrap_or(location).into(),
                message: lines.collect::<Vec<_>>().join("\n"),
            });
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerObservation {
    pub process: ProcessObservation,
    pub protocol_issues: Vec<Issue>,
    pub progress: Option<Progress>,
    pub error: Option<WorkerError>,
    pub semantic: Option<SemanticComparison>,
    pub diagnostics: DiagnosticObservation,
    pub panic: Option<PanicEvidence>,
    pub stdout_excerpt: Option<String>,
    pub stderr: String,
    pub agreement: bool,
}

fn context_matches(
    context: &Context,
    stage: Stage,
    request: &WorkerRequest,
    identity: &Identity,
    argv: &[String],
) -> bool {
    context.requested == *request
        && context.compiled == *identity
        && context.argv == argv
        && ((stage == Stage::Validate
            && context.observed.case_sha256.is_none()
            && context.identity_verified.is_none())
            || (stage == Stage::Validate
                && context.observed.case_sha256.is_some()
                && context.identity_verified == Some(false)
                && context.verify().is_err())
            || (context.identity_verified == Some(true) && context.verify().is_ok()))
}

fn diagnostics(case: &Case, header: &Header, root: &Path, required: bool) -> DiagnosticObservation {
    let mut issues = Vec::new();
    let record = read_record::<Diagnostics>(root, "diagnostics.json", required, &mut issues);
    if let Some(facts) = &record.value {
        let mut check = |valid: bool, message: &str| {
            if !valid {
                issue(
                    &mut issues,
                    IssueKind::InvalidDiagnostics,
                    "diagnostics.json",
                    message,
                );
            }
        };
        check(
            facts.header == *header,
            "diagnostic header differs from the requested case or build",
        );
        let known: BTreeSet<_> = case.documents.iter().map(|doc| &doc.id).collect();
        let stored: BTreeSet<_> = facts.stored_ids.iter().map(|entry| &entry.id).collect();
        let doc_ids: BTreeSet<_> = facts.stored_ids.iter().map(|entry| entry.doc_id).collect();
        check(
            facts.segment_count == 1
                && facts.deleted_docs == 0
                && facts.max_doc as usize == known.len()
                && facts.stored_ids.len() == known.len()
                && stored == known
                && doc_ids == (0..facts.max_doc).collect(),
            "stored document mapping or segment layout differs from the case",
        );
        let selected: Vec<_> = known
            .iter()
            .filter(|id| case.filter.selected(id))
            .map(|id| (*id).clone())
            .collect();
        check(
            facts.filter == case.filter && facts.selected_ids == selected,
            "filter or selected IDs differ from the case",
        );
        check(
            facts.sidecar_requested == case.sidecar
                && facts.sidecar_used == (case.sidecar && facts.sidecar_available)
                && (!case.sidecar || facts.sidecar_available),
            "sidecar state differs from the case or available bytes",
        );
    }
    DiagnosticObservation {
        facts: if issues.is_empty() {
            record.value
        } else {
            None
        },
        issues,
    }
}

pub fn observe_worker(
    case: &Case,
    expected: &[String],
    request: &WorkerRequest,
    identity: &Identity,
    process: ProcessObservation,
    logs: &Path,
) -> Result<WorkerObservation> {
    identity.validate()?;
    let header = Header {
        protocol: identity.protocol,
        build_record_sha256: request.build_record_sha256.clone(),
        engine_revision: request.engine_revision.clone(),
        case_sha256: request.case_sha256.clone(),
    };
    let root = Path::new(&request.output);
    let mut issues = Vec::new();
    let stdout = fs::read(logs.join("stdout"))?;
    let stderr = String::from_utf8_lossy(&fs::read(logs.join("stderr"))?).into_owned();
    if !stdout.is_empty() {
        issue(
            &mut issues,
            IssueKind::UnexpectedStdout,
            "stdout",
            "worker wrote to stdout",
        );
    }
    let progress = read_record::<Progress>(root, "stage.json", true, &mut issues)
        .value
        .filter(|progress| {
            let valid = context_matches(
                &progress.context,
                progress.stage,
                request,
                identity,
                &process.invocation.argv,
            );
            if !valid {
                issue(
                    &mut issues,
                    IssueKind::ContextMismatch,
                    "stage.json",
                    "progress context differs from the invocation",
                );
            }
            valid
        });
    let error_record = read_record::<WorkerError>(root, "error.json", false, &mut issues);
    let error = error_record.value.filter(|error| {
        let valid = context_matches(
            &error.context,
            error.stage,
            request,
            identity,
            &process.invocation.argv,
        ) && progress
            .as_ref()
            .is_some_and(|p| p.stage == error.stage && p.context == error.context);
        if !valid {
            issue(
                &mut issues,
                IssueKind::ContextMismatch,
                "error.json",
                "error context differs from progress or invocation",
            );
        }
        valid
    });
    if process.success() && error_record.present {
        issue(
            &mut issues,
            IssueKind::UnexpectedError,
            "error.json",
            "successful worker also wrote an error record",
        );
    }
    let result = read_record::<Completed>(root, "result.json", process.success(), &mut issues);
    let completed = process.success()
        && !error_record.present
        && progress.as_ref().is_some_and(|p| p.stage == Stage::Result);
    if result.present && !completed {
        issue(
            &mut issues,
            IssueKind::ResultWithoutCompletion,
            "result.json",
            "result lacks a successful exit and completed stage",
        );
    }
    let mut semantic = None;
    if let Some(result) = result.value {
        let known: BTreeSet<_> = case.documents.iter().map(|doc| &doc.id).collect();
        let canonical = result.actual.windows(2).all(|pair| pair[0] < pair[1]);
        let recognized = result.actual.iter().all(|id| known.contains(id));
        if result.header != header {
            issue(
                &mut issues,
                IssueKind::HeaderMismatch,
                "result.json",
                "result header differs from the requested case or build",
            );
        }
        if !canonical {
            issue(
                &mut issues,
                IssueKind::NoncanonicalIds,
                "result.json",
                "result IDs are repeated or not sorted",
            );
        }
        if !recognized {
            issue(
                &mut issues,
                IssueKind::UnknownId,
                "result.json",
                "result contains an unknown stable ID",
            );
        }
        if completed && result.header == header && canonical && recognized {
            let difference = longhand::compare(expected, &result.actual);
            semantic = Some(SemanticComparison {
                actual: result.actual,
                missing: difference.missing,
                spurious: difference.spurious,
            });
        }
    }
    let diagnostics_required = process.success()
        || progress
            .as_ref()
            .is_some_and(|p| matches!(p.stage, Stage::Query | Stage::Result));
    let diagnostics = diagnostics(case, &header, root, diagnostics_required);
    if !diagnostics.issues.is_empty() {
        issue(
            &mut issues,
            IssueKind::DiagnosticsRejected,
            "diagnostics.json",
            "diagnostics were not admitted",
        );
    }
    let agreement = process.success()
        && issues.is_empty()
        && diagnostics.facts.is_some()
        && semantic
            .as_ref()
            .is_some_and(|s| s.missing.is_empty() && s.spurious.is_empty());
    let panic = panic_evidence(&process, &stderr);
    Ok(WorkerObservation {
        process,
        protocol_issues: issues,
        progress,
        error,
        semantic,
        diagnostics,
        panic,
        stdout_excerpt: if stdout.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&stdout[..stdout.len().min(4096)]).into_owned())
        },
        stderr,
        agreement,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerEntry {
    pub executable: String,
    pub executable_sha256: String,
    pub build_record: String,
    pub build_record_sha256: String,
    pub engine_revision: String,
}

impl WorkerEntry {
    fn identity(&self) -> Identity {
        Identity {
            protocol: protocol::PROTOCOL_VERSION,
            build_record_sha256: Some(self.build_record_sha256.clone()),
            engine_revision: Some(self.engine_revision.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkersManifest {
    pub version: u32,
    pub reference: WorkerEntry,
    pub baseline: WorkerEntry,
    pub repaired: WorkerEntry,
}

impl WorkersManifest {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("unsupported worker manifest version".into());
        }
        for entry in [&self.reference, &self.baseline, &self.repaired] {
            entry.identity().validate()?;
            if entry.executable_sha256.len() != 64
                || !entry
                    .executable_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("invalid executable digest".into());
            }
        }
        if self.baseline.executable_sha256 == self.repaired.executable_sha256
            || self.baseline.engine_revision == self.repaired.engine_revision
        {
            return Err(
                "baseline and repaired require different executables and engine revisions".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceIdentity {
    pub executable: String,
    pub executable_sha256: String,
    pub compiled: Identity,
}

impl ReferenceIdentity {
    fn current() -> Result<Self> {
        let executable = std::env::current_exe()?;
        // The path identifies a file, not independent proof of the loaded image.
        Ok(Self {
            executable: path_string(&executable)?,
            executable_sha256: protocol::sha256(&fs::read(&executable)?),
            compiled: Identity::compiled(),
        })
    }

    pub fn verify(&self, entry: &WorkerEntry) -> Result<()> {
        self.compiled.validate()?;
        if self.compiled != entry.identity() || self.executable_sha256 != entry.executable_sha256 {
            return Err(
                "parent reference executable or compiled identity differs from the manifest".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorker {
    pub entry: WorkerEntry,
    pub executable: String,
    pub build_record: String,
    pub identity: Identity,
    pub identity_process: ProcessObservation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkers {
    pub reference: VerifiedWorker,
    pub baseline: VerifiedWorker,
    pub repaired: VerifiedWorker,
}

fn path_string(path: &Path) -> Result<String> {
    Ok(path.to_str().ok_or("path is not UTF-8")?.into())
}

fn preserve_worker(
    role: &str,
    entry: &WorkerEntry,
    source: &Path,
    root: &Path,
    timeout: Duration,
    copies: &mut BTreeMap<String, PathBuf>,
) -> Result<VerifiedWorker> {
    let record_bytes = fs::read(source.join(&entry.build_record))?;
    if protocol::sha256(&record_bytes) != entry.build_record_sha256 {
        return Err(format!("{role} build record digest differs from the manifest").into());
    }
    let record: serde_json::Value = serde_json::from_slice(&record_bytes)?;
    if record.get("version").and_then(|value| value.as_u64()) != Some(1)
        || record
            .get("engine_revision")
            .and_then(|value| value.as_str())
            != Some(&entry.engine_revision)
    {
        return Err(format!(
            "{role} build record version or engine revision differs from the manifest"
        )
        .into());
    }
    let record_path = root.join(format!("{}.json", entry.build_record_sha256));
    if !record_path.exists() {
        fs::write(&record_path, record_bytes)?;
    }
    let executable_source = source.join(&entry.executable);
    if protocol::sha256(&fs::read(&executable_source)?) != entry.executable_sha256 {
        return Err(format!("{role} executable digest differs from the manifest").into());
    }
    let executable = if let Some(path) = copies.get(&entry.executable_sha256) {
        path.clone()
    } else {
        let path = root.join(&entry.executable_sha256);
        // copy preserves the executable permission bits.
        fs::copy(&executable_source, &path)?;
        if protocol::sha256(&fs::read(&path)?) != entry.executable_sha256 {
            return Err(format!("{role} executable digest differs from the manifest").into());
        }
        copies.insert(entry.executable_sha256.clone(), path.clone());
        path
    };
    let logs = root.join(format!("{role}-identity"));
    let argv = vec![path_string(&executable)?, "identity".into()];
    let process = supervise(&argv, root, &logs, timeout)?;
    if !process.success() {
        return Err(format!(
            "{role} identity operation failed: status {:?}, timed_out {}, orchestration_error {:?}",
            process.waited, process.timed_out, process.orchestration_error
        )
        .into());
    }
    let identity: Identity = serde_json::from_slice(&fs::read(logs.join("stdout"))?)?;
    if identity != entry.identity() {
        return Err(format!("{role} compiled identity differs from the manifest").into());
    }
    Ok(VerifiedWorker {
        entry: entry.clone(),
        executable: path_string(&executable)?,
        build_record: path_string(&record_path)?,
        identity,
        identity_process: process,
    })
}

pub fn preflight(
    manifest: &WorkersManifest,
    source: &Path,
    root: &Path,
    reference: &ReferenceIdentity,
    timeout: Duration,
) -> Result<VerifiedWorkers> {
    manifest.validate()?;
    reference.verify(&manifest.reference)?;
    fs::create_dir(root)?;
    let mut copies = BTreeMap::new();
    Ok(VerifiedWorkers {
        reference: preserve_worker(
            "reference",
            &manifest.reference,
            source,
            root,
            timeout,
            &mut copies,
        )?,
        baseline: preserve_worker(
            "baseline",
            &manifest.baseline,
            source,
            root,
            timeout,
            &mut copies,
        )?,
        repaired: preserve_worker(
            "repaired",
            &manifest.repaired,
            source,
            root,
            timeout,
            &mut copies,
        )?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    pub version: u32,
    pub cases: Vec<String>,
    pub calibration: Option<calibration::Plan>,
}

pub fn replay_input(input: &Path, bytes: &[u8]) -> Result<(Vec<PathBuf>, Option<Corpus>)> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes);
    if value
        .as_ref()
        .is_ok_and(|value| value.get("cases").is_some())
    {
        let corpus: Corpus = serde_json::from_slice(bytes)?;
        if corpus.version != 1 || corpus.cases.is_empty() {
            return Err("corpus requires version 1 and at least one case".into());
        }
        let source = input.parent().ok_or("input has no parent directory")?;
        let paths = corpus
            .cases
            .iter()
            .map(|case| {
                if case.is_empty() || !Path::new(&case).is_relative() {
                    Err("corpus case paths must be nonempty and relative".into())
                } else {
                    Ok(source.join(case))
                }
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((paths, Some(corpus)))
    } else {
        Ok((vec![input.to_owned()], None))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceOutcome {
    Unreadable {
        message: String,
    },
    Malformed {
        message: String,
    },
    Rejected {
        reason: Rejection,
    },
    Answered {
        answer: longhand::Answer,
    },
    ExpectationMismatch {
        answer: longhand::Answer,
        recorded: Vec<String>,
    },
    Failed {
        message: String,
    },
}

pub fn evaluate_reference(
    case: &Case,
    evaluate: fn(&Case) -> std::result::Result<longhand::Answer, Rejection>,
) -> ReferenceOutcome {
    match std::panic::catch_unwind(|| evaluate(case)) {
        Ok(Ok(answer)) if answer.expected == case.expected.ids => {
            ReferenceOutcome::Answered { answer }
        }
        Ok(Ok(answer)) => ReferenceOutcome::ExpectationMismatch {
            answer,
            recorded: case.expected.ids.clone(),
        },
        Ok(Err(reason)) => ReferenceOutcome::Rejected { reason },
        Err(payload) => ReferenceOutcome::Failed {
            message: payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).into()))
                .unwrap_or_else(|| "reference panicked with a non-string payload".into()),
        },
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseObservation {
    pub source: String,
    pub preserved: Option<String>,
    pub case_sha256: Option<String>,
    pub case: Option<Case>,
    pub reference: ReferenceOutcome,
    pub baseline: Option<WorkerObservation>,
    pub repaired: Option<WorkerObservation>,
}

fn preserve_case(
    source: &Path,
    bytes: std::io::Result<Vec<u8>>,
    root: &Path,
    evaluate: fn(&Case) -> std::result::Result<longhand::Answer, Rejection>,
) -> Result<CaseObservation> {
    fs::create_dir(root)?;
    let mut observation = CaseObservation {
        source: path_string(source)?,
        preserved: None,
        case_sha256: None,
        case: None,
        reference: ReferenceOutcome::Unreadable {
            message: String::new(),
        },
        baseline: None,
        repaired: None,
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            observation.reference = ReferenceOutcome::Unreadable {
                message: error.to_string(),
            };
            return Ok(observation);
        }
    };
    let path = root.join("case.json");
    fs::write(&path, &bytes)?;
    observation.preserved = Some(path_string(&path)?);
    observation.case_sha256 = Some(protocol::sha256(&bytes));
    match serde_json::from_slice::<Case>(&bytes) {
        Ok(case) => {
            observation.reference = evaluate_reference(&case, evaluate);
            observation.case = Some(case);
        }
        Err(error) => {
            observation.reference = ReferenceOutcome::Malformed {
                message: error.to_string(),
            }
        }
    }
    Ok(observation)
}

fn worker_argv(executable: &str, request: &WorkerRequest) -> Vec<String> {
    vec![
        executable.into(),
        "worker".into(),
        "--case".into(),
        request.case.clone(),
        "--case-sha256".into(),
        request.case_sha256.clone(),
        "--build-record-sha256".into(),
        request.build_record_sha256.clone(),
        "--engine-revision".into(),
        request.engine_revision.clone(),
        "--output".into(),
        request.output.clone(),
    ]
}

fn run_worker(
    worker: &VerifiedWorker,
    case: &CaseObservation,
    logs: &Path,
    timeout: Duration,
) -> Result<WorkerObservation> {
    let ReferenceOutcome::Answered { answer } = &case.reference else {
        return Err("worker requires a answered case".into());
    };
    let request = WorkerRequest {
        case: case.preserved.clone().ok_or("case was not preserved")?,
        case_sha256: case.case_sha256.clone().ok_or("case digest is absent")?,
        build_record_sha256: worker.entry.build_record_sha256.clone(),
        engine_revision: worker.entry.engine_revision.clone(),
        output: path_string(&logs.join("worker"))?,
    };
    let argv = worker_argv(&worker.executable, &request);
    let process = supervise(
        &argv,
        logs.parent().ok_or("logs have no parent")?,
        logs,
        timeout,
    )?;
    observe_worker(
        case.case.as_ref().ok_or("parsed case is absent")?,
        &answer.expected,
        &request,
        &worker.identity,
        process,
        logs,
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunReport {
    pub version: u32,
    pub invocation: Invocation,
    pub input: String,
    pub manifest: String,
    pub canonical_input: Option<String>,
    pub canonical_manifest: Option<String>,
    #[serde(default)]
    pub corpus: Option<Corpus>,
    pub reference: Option<ReferenceIdentity>,
    pub workers: Option<VerifiedWorkers>,
    pub cases: Vec<CaseObservation>,
    pub complete: bool,
    pub reference_failed: bool,
    pub error: Option<String>,
}

impl RunReport {
    pub fn exit_code(&self) -> u8 {
        if !self.complete {
            1
        } else if self.reference_failed {
            2
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, clap::Args)]
pub struct ReplayRequest {
    pub input: PathBuf,
    #[arg(long)]
    pub workers: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout_seconds: u64,
}

fn replay_inner(
    request: &ReplayRequest,
    root: &Path,
    report: &mut RunReport,
    reference: impl FnOnce() -> Result<ReferenceIdentity>,
    evaluate: fn(&Case) -> std::result::Result<longhand::Answer, Rejection>,
) -> Result<()> {
    let timeout = Duration::from_secs(request.timeout_seconds);
    let input = request.input.canonicalize()?;
    report.canonical_input = Some(path_string(&input)?);
    let input_bytes = fs::read(&input)?;
    fs::write(root.join("input.json"), &input_bytes)?;
    let (paths, corpus) = replay_input(&input, &input_bytes)?;
    report.corpus = corpus;
    let manifest_path = request.workers.canonicalize()?;
    report.canonical_manifest = Some(path_string(&manifest_path)?);
    let manifest_bytes = fs::read(&manifest_path)?;
    fs::write(root.join("workers.json"), &manifest_bytes)?;
    let manifest: WorkersManifest = serde_json::from_slice(&manifest_bytes)?;
    let reference = reference()?;
    report.reference = Some(reference.clone());
    report.workers = Some(preflight(
        &manifest,
        manifest_path.parent().ok_or("manifest has no parent")?,
        &root.join("workers"),
        &reference,
        timeout,
    )?);
    let cases_root = root.join("cases");
    fs::create_dir(&cases_root)?;
    for (number, source) in paths.into_iter().enumerate() {
        let case_root = cases_root.join(format!("{number:04}"));
        let bytes = if source == input {
            Ok(input_bytes.clone())
        } else {
            fs::read(&source)
        };
        let case = preserve_case(&source, bytes, &case_root, evaluate)?;
        report.reference_failed |= matches!(case.reference, ReferenceOutcome::Failed { .. });
        report.cases.push(case);
        let case = report.cases.last_mut().ok_or("case report is absent")?;
        if matches!(case.reference, ReferenceOutcome::Answered { .. }) {
            let workers = report
                .workers
                .as_ref()
                .ok_or("verified workers are absent")?;
            for (role, worker) in [
                ("baseline", &workers.baseline),
                ("repaired", &workers.repaired),
            ] {
                let observation = run_worker(worker, case, &case_root.join(role), timeout)?;
                let error = observation.process.orchestration_error.clone();
                if role == "baseline" {
                    case.baseline = Some(observation);
                } else {
                    case.repaired = Some(observation);
                }
                if let Some(error) = error {
                    return Err(error.into());
                }
            }
        }
    }
    report.complete = true;
    Ok(())
}

pub fn replay(request: ReplayRequest, argv: Vec<String>) -> Result<u8> {
    replay_with_reference(
        request,
        argv,
        ReferenceIdentity::current,
        longhand::evaluate,
    )
}

pub fn replay_with_reference(
    request: ReplayRequest,
    argv: Vec<String>,
    reference: impl FnOnce() -> Result<ReferenceIdentity>,
    evaluate: fn(&Case) -> std::result::Result<longhand::Answer, Rejection>,
) -> Result<u8> {
    let timeout_ms = request
        .timeout_seconds
        .checked_mul(1000)
        .ok_or("timeout is too large")?;
    if timeout_ms == 0 {
        return Err("timeout must be positive".into());
    }
    fs::create_dir(&request.output)?;
    let root = request.output.canonicalize()?;
    let mut report = RunReport {
        version: 1,
        invocation: Invocation {
            argv,
            cwd: path_string(&std::env::current_dir()?)?,
            environment: "inherited".into(),
            timeout_ms,
        },
        input: path_string(&request.input)?,
        manifest: path_string(&request.workers)?,
        canonical_input: None,
        canonical_manifest: None,
        corpus: None,
        reference: None,
        workers: None,
        cases: Vec::new(),
        complete: false,
        reference_failed: false,
        error: None,
    };
    if let Err(error) = replay_inner(&request, &root, &mut report, reference, evaluate) {
        report.error = Some(error.to_string());
    }
    protocol::write_json(&root.join("report.json"), &report)?;
    if let Some(calibration) = calibration::evaluate(&report)? {
        protocol::write_json(&root.join("calibration.json"), &calibration)?;
    }
    if let Some(error) = &report.error {
        eprintln!("{error}");
    }
    Ok(report.exit_code())
}
