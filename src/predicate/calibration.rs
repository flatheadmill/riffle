use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use super::case::{Case, Geometry, Predicate, SurfaceKind, LONGHAND_VERSION};
use super::longhand::{Answer, Work};
use super::protocol::{self, Diagnostics, Result, Seed, Stage};
use super::runner::{
    CaseObservation, PanicEvidence, ReferenceOutcome, RunReport, SemanticComparison,
    WorkerObservation,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    Crossing,
    Containment,
    Split,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    CrossingPositive,
    CrossingDisjoint,
    ContainmentPositive,
    ContainmentDisjoint,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Primary {
    pub path: String,
    pub surface: SurfaceKind,
    pub mechanism: Mechanism,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub path: String,
    pub surface: SurfaceKind,
    pub kind: ControlKind,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub path: String,
    pub surface: SurfaceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub baseline_revision: String,
    pub repaired_revision: String,
    pub primaries: Vec<Primary>,
    pub controls: Vec<Control>,
    pub observations: Vec<Observation>,
}

impl Plan {
    fn paths(&self) -> impl Iterator<Item = (&str, SurfaceKind)> {
        self.primaries
            .iter()
            .map(|p| (p.path.as_str(), p.surface))
            .chain(self.controls.iter().map(|c| (c.path.as_str(), c.surface)))
            .chain(
                self.observations
                    .iter()
                    .map(|o| (o.path.as_str(), o.surface)),
            )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    BuildsUnavailable,
    CaseMissing,
    CaseUnanswered,
    ComparisonUnavailable,
    ControlFailed,
    ControlMatrix,
    ControlShapeMismatch,
    CorpusMismatch,
    DiagnosticsDivergence,
    DiagnosticsShapeMismatch,
    DiagnosticsUnavailable,
    DuplicatePlanPath,
    InvalidPlanPath,
    PanicUnbound,
    PlanVersion,
    PredicateMismatch,
    PrimaryMatrix,
    ProtocolViolation,
    ReferenceRevisionMismatch,
    RepairedFailed,
    RevisionMismatch,
    RunIncomplete,
    SeedIneligible,
    SurfaceMismatch,
    UnexpectedDifference,
    UnexpectedPanic,
    UnexpectedRoute,
    WorkMismatch,
    WorkerError,
    WorkerExit,
    WorkerMissing,
    WorkerTimeout,
}

type Reasons = BTreeSet<Reason>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Reproduced,
    NotReproduced,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgreementOutcome {
    Agrees,
    Disagrees,
    Inconclusive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agreement {
    pub outcome: AgreementOutcome,
    pub reasons: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedEvidence {
    pub member: usize,
    pub seed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanicBounds {
    pub len: u64,
    pub index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryEvidence {
    pub raw_members: Vec<usize>,
    pub eligible_seeds: Vec<SeedEvidence>,
    pub baseline_difference: Option<SemanticComparison>,
    pub baseline_panic: Option<PanicEvidence>,
    pub panic_bounds: Option<PanicBounds>,
    pub observed_pattern: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimaryResult {
    pub path: String,
    pub surface: SurfaceKind,
    pub mechanism: Mechanism,
    pub verdict: Verdict,
    pub reasons: Reasons,
    pub evidence: PrimaryEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResult {
    pub path: String,
    pub surface: SurfaceKind,
    pub baseline: Agreement,
    pub repaired: Agreement,
    pub passed: bool,
    pub reasons: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairedResult {
    pub path: String,
    pub surface: SurfaceKind,
    pub agreement: Agreement,
    pub reasons: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceResult {
    pub surface: SurfaceKind,
    pub qualified: bool,
    pub controls_passed: bool,
    pub mechanisms_reproduced: bool,
    pub repaired_agrees: bool,
    pub cases_consistent: bool,
    pub reasons: Reasons,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    pub version: u32,
    pub report: String,
    pub report_sha256: String,
    pub baseline_revision: Option<String>,
    pub repaired_revision: Option<String>,
    pub reference_revision: Option<String>,
    pub reasons: Reasons,
    pub controls: Vec<ControlResult>,
    pub primaries: Vec<PrimaryResult>,
    pub repaired: Vec<RepairedResult>,
    pub surfaces: Vec<SurfaceResult>,
    pub qualified: bool,
}

const SURFACES: [SurfaceKind; 2] = [SurfaceKind::Plane, SurfaceKind::Sphere];
const MECHANISMS: [Mechanism; 3] = [
    Mechanism::Crossing,
    Mechanism::Containment,
    Mechanism::Split,
];
const CONTROLS: [ControlKind; 4] = [
    ControlKind::CrossingPositive,
    ControlKind::CrossingDisjoint,
    ControlKind::ContainmentPositive,
    ControlKind::ContainmentDisjoint,
];

fn plan_reasons(report: &RunReport, plan: &Plan, corpus: &[String]) -> Reasons {
    let mut reasons = Reasons::new();
    if plan.version != 1 {
        reasons.insert(Reason::PlanVersion);
    }
    let mut paths = BTreeSet::new();
    for (path, _) in plan.paths() {
        if path.is_empty()
            || !Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            reasons.insert(Reason::InvalidPlanPath);
        }
        if !paths.insert(path) {
            reasons.insert(Reason::DuplicatePlanPath);
        }
    }
    let corpus_paths: BTreeSet<_> = corpus.iter().map(String::as_str).collect();
    if paths != corpus_paths || corpus_paths.len() != corpus.len() {
        reasons.insert(Reason::CorpusMismatch);
    }
    for surface in SURFACES {
        for mechanism in MECHANISMS {
            if plan
                .primaries
                .iter()
                .filter(|p| p.surface == surface && p.mechanism == mechanism)
                .count()
                != 1
            {
                reasons.insert(Reason::PrimaryMatrix);
            }
        }
        for kind in CONTROLS {
            if plan
                .controls
                .iter()
                .filter(|c| c.surface == surface && c.kind == kind)
                .count()
                != 1
            {
                reasons.insert(Reason::ControlMatrix);
            }
        }
    }
    if !report.complete {
        reasons.insert(Reason::RunIncomplete);
    }
    if let Some(workers) = &report.workers {
        if workers.baseline.entry.engine_revision != plan.baseline_revision
            || workers.repaired.entry.engine_revision != plan.repaired_revision
            || plan.baseline_revision == plan.repaired_revision
        {
            reasons.insert(Reason::RevisionMismatch);
        }
        if workers.reference.entry.engine_revision != plan.repaired_revision {
            reasons.insert(Reason::ReferenceRevisionMismatch);
        }
    } else {
        reasons.insert(Reason::BuildsUnavailable);
    }
    if (report.reference.is_some() || report.workers.is_some())
        && report
            .reference
            .as_ref()
            .and_then(|r| r.compiled.engine_revision.as_deref())
            != Some(plan.repaired_revision.as_str())
    {
        reasons.insert(Reason::ReferenceRevisionMismatch);
    }
    reasons
}

fn case_evidence<'a>(
    report: &'a RunReport,
    path: &str,
) -> std::result::Result<(&'a CaseObservation, &'a Case, &'a Answer), Reason> {
    let source = report
        .canonical_input
        .as_deref()
        .and_then(|input| Path::new(input).parent())
        .ok_or(Reason::CaseMissing)?
        .join(path);
    let mut matches = report
        .cases
        .iter()
        .filter(|c| Path::new(&c.source) == source);
    let observation = matches.next().ok_or(Reason::CaseMissing)?;
    if matches.next().is_some() {
        return Err(Reason::CaseMissing);
    }
    let (Some(case), ReferenceOutcome::Answered { answer }) =
        (&observation.case, &observation.reference)
    else {
        return Err(Reason::CaseUnanswered);
    };
    if case.predicate != Predicate::Intersects || answer.version != LONGHAND_VERSION {
        return Err(Reason::PredicateMismatch);
    }
    Ok((observation, case, answer))
}

fn worker_reasons(worker: Option<&WorkerObservation>) -> Reasons {
    let mut reasons = Reasons::new();
    let Some(worker) = worker else {
        reasons.insert(Reason::WorkerMissing);
        return reasons;
    };
    if !worker.protocol_issues.is_empty() {
        reasons.insert(Reason::ProtocolViolation);
    }
    if worker.process.timed_out {
        reasons.insert(Reason::WorkerTimeout);
    }
    if worker.error.is_some() {
        reasons.insert(Reason::WorkerError);
    }
    if worker.process.orchestration_error.is_some()
        || (!worker.process.success()
            && !worker.process.timed_out
            && worker.error.is_none()
            && worker.panic.is_none())
    {
        reasons.insert(Reason::WorkerExit);
    }
    if worker.diagnostics.facts.is_none() || !worker.diagnostics.issues.is_empty() {
        reasons.insert(Reason::DiagnosticsUnavailable);
    }
    reasons
}

fn agreement(worker: Option<&WorkerObservation>) -> Agreement {
    let mut reasons = worker_reasons(worker);
    if worker.is_some_and(|w| w.panic.is_some()) {
        reasons.insert(Reason::UnexpectedPanic);
    }
    let outcome = if reasons.is_empty() {
        match worker.and_then(|w| w.semantic.as_ref()) {
            Some(s) if s.missing.is_empty() && s.spurious.is_empty() => AgreementOutcome::Agrees,
            Some(_) => AgreementOutcome::Disagrees,
            None => {
                reasons.insert(Reason::ComparisonUnavailable);
                AgreementOutcome::Inconclusive
            }
        }
    } else {
        AgreementOutcome::Inconclusive
    };
    Agreement { outcome, reasons }
}

fn query_vertices(query: &Geometry) -> Option<Vec<usize>> {
    match query {
        Geometry::Polygon { coordinates } if coordinates.len() == 1 => {
            Some(vec![coordinates[0].len()])
        }
        Geometry::MultiPolygon { coordinates } => coordinates
            .iter()
            .map(|rings| (rings.len() == 1).then(|| rings[0].len()))
            .collect(),
        _ => None,
    }
}

fn valid_seeds(case: &Case, facts: &Diagnostics) -> bool {
    let Some(vertices) = query_vertices(&case.query) else {
        return false;
    };
    facts.seeds.iter().all(|seed| {
        vertices.first() == Some(&seed.member_zero_vertices)
            && vertices.get(seed.member as usize) == Some(&seed.member_vertices)
            && seed
                .edge_indices
                .iter()
                .all(|&edge| u64::from(edge) + 1 < seed.member_vertices as u64)
            && seed.index_start <= seed.index_end
            && seed.index_end <= facts.cell_count
            && (seed.index_start < seed.index_end) == seed.first_index_level.is_some()
            && (0..=30).contains(&seed.query_level)
            && seed
                .first_index_level
                .is_none_or(|level| (0..=30).contains(&level))
    })
}

fn diagnostic_reasons(observation: &CaseObservation, case: &Case) -> Reasons {
    let mut reasons = Reasons::new();
    fn facts(worker: &Option<WorkerObservation>) -> Option<&Diagnostics> {
        worker.as_ref().and_then(|w| w.diagnostics.facts.as_ref())
    }
    let baseline = facts(&observation.baseline);
    let repaired = facts(&observation.repaired);
    if baseline.is_none() || repaired.is_none() {
        reasons.insert(Reason::DiagnosticsUnavailable);
    }
    for facts in [baseline, repaired].into_iter().flatten() {
        if !valid_seeds(case, facts) {
            reasons.insert(Reason::DiagnosticsShapeMismatch);
        }
    }
    if let (Some(baseline), Some(repaired)) = (baseline, repaired) {
        if baseline.seeds != repaired.seeds
            || baseline.clipper != repaired.clipper
            || baseline.cell_count != repaired.cell_count
            || baseline.sidecar_available != repaired.sidecar_available
        {
            reasons.insert(Reason::DiagnosticsDivergence);
        }
    }
    reasons
}

fn work_shape(work: &Work, members: usize) -> bool {
    work.crossings_by_query_member.len() == members
        && work.indexed_vertex_in_query_member.len() == members
        && work.query_member_vertex_in_indexed.len() == members
}

fn raw_members(case: &Case, answer: &Answer, primary: &Primary) -> Vec<usize> {
    let Some(vertices) = query_vertices(&case.query) else {
        return Vec::new();
    };
    let Some(work) = answer.work.get(&primary.target) else {
        return Vec::new();
    };
    let target = case.documents.iter().find(|doc| doc.id == primary.target);
    if vertices.len() < 2
        || !work_shape(work, vertices.len())
        || !work.selected
        || !work.matches_without_filter
        || work.query_member_vertex_in_indexed.iter().any(|&v| v)
        || work.crossings_by_query_member[0] != 0
        || work.indexed_vertex_in_query_member[0]
    {
        return Vec::new();
    }
    match primary.mechanism {
        Mechanism::Crossing | Mechanism::Split => {
            if work.indexed_vertex_in_query_member.iter().any(|&v| v) {
                return Vec::new();
            }
            (1..vertices.len())
                .filter(|&member| work.crossings_by_query_member[member] > 0)
                .collect()
        }
        Mechanism::Containment => {
            if !target.is_some_and(|doc| matches!(doc.geometry, Geometry::Point { .. }))
                || work.crossings_by_query_member.iter().any(|&n| n != 0)
            {
                return Vec::new();
            }
            (1..vertices.len())
                .filter(|&member| work.indexed_vertex_in_query_member[member])
                .collect()
        }
    }
}

fn seed_eligible(seed: &Seed, mechanism: Mechanism) -> bool {
    let Some(level) = seed.first_index_level else {
        return false;
    };
    if seed.index_start >= seed.index_end {
        return false;
    }
    if mechanism == Mechanism::Containment {
        return level <= seed.query_level;
    }
    let bad_zero_bounds = seed
        .edge_indices
        .iter()
        .any(|&edge| u64::from(edge) + 1 >= seed.member_zero_vertices as u64);
    level > seed.query_level
        && seed.query_level < 30
        && !seed.edge_indices.is_empty()
        && bad_zero_bounds == (mechanism == Mechanism::Split)
}

fn control_shape(case: &Case, answer: &Answer, control: &Control) -> bool {
    if query_vertices(&case.query).is_none_or(|members| members.len() != 1) {
        return false;
    }
    let Some(work) = answer.work.get(&control.target) else {
        return false;
    };
    let Some(target) = case.documents.iter().find(|doc| doc.id == control.target) else {
        return false;
    };
    if !work_shape(work, 1) || !work.selected {
        return false;
    }
    let positive = matches!(
        control.kind,
        ControlKind::CrossingPositive | ControlKind::ContainmentPositive
    );
    if work.matches_without_filter != positive || work.query_member_vertex_in_indexed[0] {
        return false;
    }
    match control.kind {
        ControlKind::CrossingPositive | ControlKind::CrossingDisjoint => {
            matches!(&target.geometry, Geometry::Polygon { coordinates } if coordinates.len() == 1)
                && (work.crossings_by_query_member[0] > 0) == positive
                && !work.indexed_vertex_in_query_member[0]
        }
        ControlKind::ContainmentPositive | ControlKind::ContainmentDisjoint => {
            matches!(target.geometry, Geometry::Point { .. })
                && work.crossings_by_query_member[0] == 0
                && work.indexed_vertex_in_query_member[0] == positive
        }
    }
}

fn check_control(report: &RunReport, control: &Control) -> ControlResult {
    let mut result = ControlResult {
        path: control.path.clone(),
        surface: control.surface,
        baseline: Agreement {
            outcome: AgreementOutcome::Inconclusive,
            reasons: Reasons::new(),
        },
        repaired: Agreement {
            outcome: AgreementOutcome::Inconclusive,
            reasons: Reasons::new(),
        },
        passed: false,
        reasons: Reasons::new(),
    };
    let (observation, case, answer) = match case_evidence(report, &control.path) {
        Ok(evidence) => evidence,
        Err(reason) => {
            result.reasons.insert(reason);
            return result;
        }
    };
    if case.surface != control.surface {
        result.reasons.insert(Reason::SurfaceMismatch);
    }
    if !control_shape(case, answer, control) {
        result.reasons.insert(Reason::ControlShapeMismatch);
    }
    result.baseline = agreement(observation.baseline.as_ref());
    result.repaired = agreement(observation.repaired.as_ref());
    result.reasons.extend(diagnostic_reasons(observation, case));
    result.passed = result.reasons.is_empty()
        && result.baseline.outcome == AgreementOutcome::Agrees
        && result.repaired.outcome == AgreementOutcome::Agrees;
    result
}

fn panic_route(panic: &PanicEvidence, worker: &WorkerObservation) -> bool {
    panic.thread == "main"
        && worker
            .progress
            .as_ref()
            .is_some_and(|p| p.stage == Stage::Query)
        && panic
            .message
            .starts_with("index out of bounds: the len is ")
}

fn panic_file(panic: &PanicEvidence) -> &str {
    panic
        .location
        .split(':')
        .next()
        .and_then(|path| path.rsplit('/').next())
        .unwrap_or("")
}

fn panic_bounds(panic: &PanicEvidence) -> Option<PanicBounds> {
    let (len, index) = panic
        .message
        .lines()
        .next()?
        .strip_prefix("index out of bounds: the len is ")?
        .split_once(" but the index is ")?;
    if !len
        .bytes()
        .chain(index.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(PanicBounds {
        len: len.parse().ok()?,
        index: index.parse().ok()?,
    })
}

fn panic_matches_seed(bounds: &PanicBounds, seed: &Seed) -> bool {
    bounds.len == seed.member_zero_vertices as u64
        && seed.edge_indices.iter().any(|&edge| {
            let edge = u64::from(edge);
            // get_edge reads vertices[edge] before vertices[edge + 1].
            edge + 1 >= bounds.len
                && bounds.index == if edge >= bounds.len { edge } else { edge + 1 }
        })
}

fn check_primary(
    report: &RunReport,
    primary: &Primary,
    global: &Reasons,
    controls_passed: bool,
) -> PrimaryResult {
    let mut result = PrimaryResult {
        path: primary.path.clone(),
        surface: primary.surface,
        mechanism: primary.mechanism,
        verdict: Verdict::Inconclusive,
        reasons: global.clone(),
        evidence: PrimaryEvidence {
            raw_members: Vec::new(),
            eligible_seeds: Vec::new(),
            baseline_difference: None,
            baseline_panic: None,
            panic_bounds: None,
            observed_pattern: false,
        },
    };
    if !controls_passed {
        result.reasons.insert(Reason::ControlFailed);
    }
    let (observation, case, answer) = match case_evidence(report, &primary.path) {
        Ok(evidence) => evidence,
        Err(reason) => {
            result.reasons.insert(reason);
            return result;
        }
    };
    if case.surface != primary.surface {
        result.reasons.insert(Reason::SurfaceMismatch);
    }
    result.evidence.raw_members = raw_members(case, answer, primary);
    if result.evidence.raw_members.is_empty() {
        result.reasons.insert(Reason::WorkMismatch);
    }
    result.reasons.extend(diagnostic_reasons(observation, case));
    result
        .reasons
        .extend(worker_reasons(observation.baseline.as_ref()));
    let Some(worker) = &observation.baseline else {
        return result;
    };
    if let Some(facts) = worker
        .diagnostics
        .facts
        .as_ref()
        .filter(|_| !result.evidence.raw_members.is_empty())
    {
        result.evidence.eligible_seeds = facts
            .seeds
            .iter()
            .enumerate()
            .filter_map(|(index, seed)| {
                let member = seed.member as usize;
                (result.evidence.raw_members.contains(&member)
                    && seed_eligible(seed, primary.mechanism))
                .then_some(SeedEvidence {
                    member,
                    seed: index,
                })
            })
            .collect();
        if result.evidence.eligible_seeds.is_empty() {
            result.reasons.insert(Reason::SeedIneligible);
        }
    }
    result.evidence.baseline_difference = worker.semantic.clone();
    result.evidence.baseline_panic = worker.panic.clone();
    result.evidence.panic_bounds = worker.panic.as_ref().and_then(panic_bounds);
    let mut verdict = Verdict::Inconclusive;
    if let Some(comparison) = &worker.semantic {
        let agrees = comparison.missing.is_empty() && comparison.spurious.is_empty();
        if primary.mechanism == Mechanism::Split {
            verdict = Verdict::NotReproduced;
        } else if comparison.missing == [primary.target.clone()] && comparison.spurious.is_empty() {
            result.evidence.observed_pattern = true;
            verdict = Verdict::Reproduced;
        } else if agrees {
            verdict = Verdict::NotReproduced;
        }
        if primary.mechanism != Mechanism::Split && !agrees && verdict != Verdict::Reproduced {
            result.reasons.insert(Reason::UnexpectedDifference);
        }
    } else if let Some(panic) = &worker.panic {
        if primary.mechanism == Mechanism::Split && panic_route(panic, worker) {
            if panic_file(panic) == "query_edge_provider.rs" {
                if !result.evidence.eligible_seeds.is_empty() {
                    let bound = result.evidence.panic_bounds.as_ref().is_some_and(|bounds| {
                        worker.diagnostics.facts.as_ref().is_some_and(|facts| {
                            result.evidence.eligible_seeds.iter().any(|evidence| {
                                panic_matches_seed(bounds, &facts.seeds[evidence.seed])
                            })
                        })
                    });
                    if bound {
                        result.evidence.observed_pattern = true;
                        verdict = Verdict::Reproduced;
                    } else {
                        result.reasons.insert(Reason::PanicUnbound);
                    }
                }
            } else {
                result.reasons.insert(Reason::UnexpectedRoute);
            }
        } else {
            result.reasons.insert(Reason::UnexpectedPanic);
        }
    } else if worker.process.success() {
        result.reasons.insert(Reason::ComparisonUnavailable);
    }
    if result.reasons.is_empty() {
        result.verdict = verdict;
    }
    result
}

fn check_repaired(report: &RunReport, path: &str, surface: SurfaceKind) -> RepairedResult {
    let mut result = RepairedResult {
        path: path.into(),
        surface,
        agreement: Agreement {
            outcome: AgreementOutcome::Inconclusive,
            reasons: Reasons::new(),
        },
        reasons: Reasons::new(),
    };
    match case_evidence(report, path) {
        Ok((observation, case, _)) => {
            result.agreement = agreement(observation.repaired.as_ref());
            if case.surface != surface {
                result.reasons.insert(Reason::SurfaceMismatch);
            }
            result.reasons.extend(diagnostic_reasons(observation, case));
        }
        Err(reason) => {
            result.reasons.insert(reason);
        }
    }
    result
}

// Classification uses the admitted report only, never files left by a child.
pub fn evaluate(report: &RunReport) -> Result<Option<Calibration>> {
    let Some(corpus) = &report.corpus else {
        return Ok(None);
    };
    let Some(plan) = &corpus.calibration else {
        return Ok(None);
    };
    let reasons = plan_reasons(report, plan, &corpus.cases);
    let controls: Vec<_> = plan
        .controls
        .iter()
        .map(|c| check_control(report, c))
        .collect();
    let controls_passed = |surface| {
        let matching: Vec<_> = controls.iter().filter(|c| c.surface == surface).collect();
        matching.len() == CONTROLS.len() && matching.iter().all(|c| c.passed)
    };
    let primaries: Vec<_> = plan
        .primaries
        .iter()
        .map(|p| check_primary(report, p, &reasons, controls_passed(p.surface)))
        .collect();
    let repaired: Vec<_> = plan
        .paths()
        .map(|(path, surface)| check_repaired(report, path, surface))
        .collect();
    let surfaces: Vec<_> = SURFACES
        .into_iter()
        .map(|surface| {
            let controls_passed = controls_passed(surface);
            let mechanisms_reproduced = primaries.iter().filter(|p| p.surface == surface).count()
                == MECHANISMS.len()
                && primaries
                    .iter()
                    .filter(|p| p.surface == surface)
                    .all(|p| p.verdict == Verdict::Reproduced);
            let repaired_agrees = repaired
                .iter()
                .filter(|r| r.surface == surface)
                .all(|r| r.agreement.outcome == AgreementOutcome::Agrees);
            let cases_consistent = repaired
                .iter()
                .filter(|r| r.surface == surface)
                .all(|r| r.reasons.is_empty());
            let mut surface_reasons = reasons.clone();
            if !controls_passed {
                surface_reasons.insert(Reason::ControlFailed);
            }
            if !repaired_agrees {
                surface_reasons.insert(Reason::RepairedFailed);
            }
            for primary in primaries.iter().filter(|p| p.surface == surface) {
                surface_reasons.extend(&primary.reasons);
            }
            for case in repaired.iter().filter(|r| r.surface == surface) {
                surface_reasons.extend(&case.reasons);
            }
            SurfaceResult {
                surface,
                controls_passed,
                mechanisms_reproduced,
                repaired_agrees,
                cases_consistent,
                qualified: surface_reasons.is_empty() && mechanisms_reproduced,
                reasons: surface_reasons,
            }
        })
        .collect();
    Ok(Some(Calibration {
        version: 1,
        report: "report.json".into(),
        report_sha256: protocol::sha256(&protocol::json_bytes(report)?),
        baseline_revision: report
            .workers
            .as_ref()
            .map(|w| w.baseline.entry.engine_revision.clone()),
        repaired_revision: report
            .workers
            .as_ref()
            .map(|w| w.repaired.entry.engine_revision.clone()),
        reference_revision: report
            .reference
            .as_ref()
            .and_then(|r| r.compiled.engine_revision.clone()),
        qualified: surfaces.iter().all(|s| s.qualified),
        reasons,
        controls,
        primaries,
        repaired,
        surfaces,
    }))
}
