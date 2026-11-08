# Predicate replay

Replay compares recorded Intersects cases with an exhaustive geometry reference and two explicit engine builds:

```sh
predicate replay cases/intersects/corpus.json \
    --workers workers.json --output runs/example --timeout-seconds 60
```

The input can also be a single case. The output directory must not exist. Replay does not build executables or discover an engine checkout.

## Inputs

A corpus fixes case order and resolves paths relative to its own directory:

```json
{
  "version": 1,
  "cases": [
    "plane_later_member_crossing.json",
    "sphere_later_member_crossing.json"
  ]
}
```

The worker manifest has exactly three roles: `reference`, `baseline` and `repaired`. Each role contains these fields:

```json
{
  "executable": "build/predicate",
  "executable_sha256": "<64 lowercase hexadecimal digits>",
  "build_record": "build/record.json",
  "build_record_sha256": "<64 lowercase hexadecimal digits>",
  "engine_revision": "<40 lowercase hexadecimal digits>"
}
```

The enclosing object also has `"version": 1`. Paths resolve relative to the worker manifest. Digests identify file bytes, including whitespace in JSON. Build records must have version 1 and the matching `engine_revision`; their other fields describe the controlled build. An executable digest alone is not source provenance.

The running parent must match the reference role's executable digest and compiled identity. Reference may explicitly repeat the repaired entry. Baseline and repaired must have different executable digests and different engine revisions. There is no implicit current-executable worker. Unstamped binaries fail preflight.

Replay preserves executable copies and build records, verifies the copies and runs their identity operation before launching cases. Every declared source path is checked, including roles that share executable bytes. One preserved copy of each executable serves the entire run. The current executable path identifies a file; hashing it does not independently prove the identity of the already loaded image. Keep executables, manifests and case inputs unchanged during a run.

## Observations

Original input, manifest and case bytes are preserved before evaluation. Numbered case directories retain corpus order. The parent evaluates the raw reference once per parsed case. Invalid, unsupported, ambiguous, malformed, unreadable and failed inputs remain explicit outcomes. A reference answer that differs from the recorded expectation also skips the engines. An empty answer still runs both workers.

Each worker gets a fresh index and its own process. Arguments and paths are recorded. The child inherits the environment, receives no stdin, and writes stdout and stderr to files. The positive timeout applies separately to every identity and worker invocation; its default is 60 seconds. On timeout or a supervision error, the parent kills and waits for the child. This covers the child only; predicate workers spawn no other processes.

`report.json` keeps process status, protocol issues, semantic comparison and diagnostic admission separate. A semantic comparison requires exit zero, matching requested and compiled identities, the observed case digest, the final result stage, and unique known IDs in sorted order. It reports both missing and spurious IDs. A known ID excluded by the filter is a spurious hit.

Invalid or absent required diagnostics prevent agreement without discarding an independently valid comparison. Diagnostics are not required before the query stage on a failed setup. Noisy stdout also prevents agreement; its first 4096 bytes are included as an excerpt, with the full bytes retained separately. A result file after a timeout, nonzero exit or incomplete stage is never compared.

Panic evidence requires exit 101 and a standard Rust panic line. The report retains the thread, source location, message, last admitted stage and diagnostic facts. That evidence describes the observed failure; it does not by itself establish a particular historical defect. The raw files remain available when records fail admission.

## Exit status

- `0`: Every supplied case has an outcome and every launched worker has an observation. Engine disagreements, panics and timeouts can occur in this report.
- `1`: Invocation, preflight or orchestration failed. When the output directory could be created, the report records the failure and any earlier observations.
- `2`: The run completed, but the reference itself failed. Read the reference failures in the report.

Exit zero does not mean that the engines agree. Read the per-worker `agreement` fields. Historical calibration is a separate determination.

The parent catches unwinding panics only around reference evaluation. Worker search panics remain uncaught, so their process exits and stderr can be examined. The [worker protocol](predicate-worker.md) describes the fresh-index route and its records.

## Historical calibration

The recorded Intersects corpus includes an optional `calibration` plan. Its version is 1. It declares `baseline_revision`, `repaired_revision`, `primaries`, `controls` and `observations`. A primary has `path`, `surface`, `mechanism` and `target`; a control has `path`, `surface`, `kind` and `target`; an observation has `path` and `surface`. The checked-in [corpus](../cases/intersects/corpus.json) is a complete example.

The plan declares obligations. It does not choose which evidence counts. Each surface requires exactly one primary for each mechanism (`crossing`, `containment`, `split`) and one control of each kind (`crossing_positive`, `crossing_disjoint`, `containment_positive`, `containment_disjoint`). Plan paths must be unique, consist of normal relative path components, and equal the corpus path set. Observation counts may grow without changing these obligations.

The pins must match the verified baseline and repaired builds. The reference must use the repaired revision. A malformed plan prevents replay. An incomplete or inconsistent but well-formed plan still permits ordinary replay and yields inconclusive calibration. Without a plan there is no historical claim or calibration artifact. The plan is captured before worker preflight, so a preflight failure can still produce an inconclusive calibration record.

`calibration.json` names `report.json` and its SHA-256 digest, including the final newline. Keep the two files together. Classification uses only admitted evidence retained in that report, without rereading child directories. It records actual baseline, repaired and reference revisions; per-primary verdicts; control outcomes; repaired agreement for every declared case; and qualification for each surface. Reasons are sorted and unique. An unavailable or unanswered case stops dependent checks; independent plan, build and control reasons remain visible.

A crossing primary requires a selected target with proper crossings in a later query member, no crossing in member zero, and no containment in either direction. The same later member must have a seed with edges, a nonempty candidate range and a first indexed level greater than its query level. Its edge endpoints must fit member zero's bounds. A containment primary requires an indexed Point inside a later query member and outside member zero, without crossings or reverse containment. Its matching seed must have a nonempty candidate range and a first indexed level no greater than its query level.

A split primary has the crossing-only evidence and a split-eligible seed whose edge endpoints exceed member zero's bounds. Seed member sizes and edge indices must fit their actual owning raw member. Initial seeds describe entry eligibility, not an execution trace. Baseline and repaired seed vectors, clipper settings, cell counts and sidecar availability must match. Admission checks each worker's header, segment layout, stable IDs, filter and selected IDs against its case and build. Segment identifiers and process paths may differ, as may the recovered mapping from stable IDs to document numbers.

For crossing and containment, reproduction requires a completed baseline result missing exactly the target with no spurious hits. For split, it requires an admitted query-stage, main-thread Rust bounds panic at `query_edge_provider.rs`. The parsed length must equal member zero's vertex count, and the parsed index must match the first out-of-bounds endpoint read of an eligible seed edge. The evidence retains both numbers in `panic_bounds`. A panic elsewhere in that file, such as an invalid member lookup, does not meet this binding merely by naming the file. A bounds panic at `intersects.rs` is an unexpected route. Full comparisons and panic evidence remain recorded even when calibration is inconclusive. Every single-member control must have its declared raw geometry and evidence shape and must agree on both builds. A disjoint point control tests that a point outside the query is excluded.

Primary verdicts are:

- `reproduced`: The intended baseline failure, raw geometry evidence, seeds, build bindings and surface controls meet their obligations.
- `not_reproduced`: An eligible baseline run agrees for a miss primary, or completes for a split primary. A completed split's semantic difference is recorded separately.
- `inconclusive`: Missing evidence or a failed obligation prevents the historical determination.

`observed_pattern` records the expected baseline outcome even if another obligation prevents `reproduced`. It does not imply that the repaired engine agrees. Repaired agreement is reported separately. Agreement outcomes are `agrees` for an admitted equal answer, `disagrees` for an admitted unequal answer, and `inconclusive` when the worker cannot establish agreement. A control's `passed` flag requires its raw shape, both agreements and consistent diagnostics.

A surface is `qualified` only when the plan and build bindings are valid, all four controls pass, all three mechanisms reproduce, all its repaired cases agree, and the case evidence checks pass. `cases_consistent` covers case presence, raw admission, declared surface and diagnostic consistency. Both surfaces must qualify for overall qualification. Baseline outcomes for observation cases do not have to agree. Calibration does not change replay's exit status.

## Calibration reasons

These names form the closed reason vocabulary. The report supplies the underlying comparison, protocol issue, input outcome or process evidence.

| Reason | Meaning |
| --- | --- |
| `builds_unavailable` | Worker preflight did not establish all three build identities. |
| `case_missing` | The report has no unique observation for the declared case path. |
| `case_unanswered` | The raw reference did not admit the case with its recorded expectation. |
| `comparison_unavailable` | A successful worker has no admitted semantic comparison. |
| `control_failed` | The surface's four control obligations did not all pass. |
| `control_matrix` | The plan lacks exactly one control of each kind on each surface. |
| `control_shape_mismatch` | A control's raw geometry or evidence does not test its declared obligation. |
| `corpus_mismatch` | Plan paths differ from corpus paths, or corpus paths repeat. |
| `diagnostics_divergence` | Admitted seeds, clipper settings, cell counts or sidecar availability differ between builds. |
| `diagnostics_shape_mismatch` | Seed member sizes, edge indices, index ranges or levels are inconsistent. |
| `diagnostics_unavailable` | One or both workers lack admitted diagnostic facts. |
| `duplicate_plan_path` | The plan declares the same path more than once. |
| `invalid_plan_path` | A plan path is empty or contains a non-normal path component. |
| `panic_unbound` | The bounds message cannot be parsed or does not match an eligible seed's first failing endpoint. |
| `plan_version` | The calibration plan has an unsupported version. |
| `predicate_mismatch` | Historical Intersects calibration received another predicate or reference version. |
| `primary_matrix` | The plan lacks exactly one primary of each mechanism on each surface. |
| `protocol_violation` | The worker observation contains a protocol issue. |
| `reference_revision_mismatch` | The actual reference revision does not match the repaired pin. |
| `repaired_failed` | At least one required repaired case lacks agreement. |
| `revision_mismatch` | Worker revisions differ from the declared pins, or both pins are equal. |
| `run_incomplete` | Replay did not finish the corpus. |
| `seed_ineligible` | No seed of the raw geometry evidence member meets the mechanism's preconditions. |
| `surface_mismatch` | The case surface differs from its plan declaration. |
| `unexpected_difference` | A miss primary has a completed difference other than the sole target miss. |
| `unexpected_panic` | A panic does not have the intended mechanism, thread, stage or message. |
| `unexpected_route` | A query-stage main-thread bounds panic came from another source file. |
| `work_mismatch` | The target lacks the required raw geometric evidence. |
| `worker_error` | The worker recorded an ordinary setup or query error. |
| `worker_exit` | The process failed without recognized panic, timeout or ordinary error evidence, or supervision failed. |
| `worker_missing` | The report has no worker observation for the role. |
| `worker_timeout` | The worker exceeded its deadline. |
