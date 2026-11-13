//! Spatial query executor.
//!
//! Evaluates multi-stage spatial queries during Weight construction, producing per-segment
//! bitsets or scored result sets that per-segment scorers iterate.

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::fmt;

use common::BitSet;
use serde_json::Value;
use tantivy::index::SegmentId;
use tantivy::query::{BitSetDocSet, EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::schema::Field;
use tantivy::{DocId, DocSet, Score, Searcher, SegmentReader, TERMINATED};
use tantivy_spatial::cell_index_reader::CellIndexReader;

use super::spatial::{field_index, GeometryInput, PreparedSpatialQuery};
use super::SpatialPredicate;
use crate::spatial::SpatialFieldReader;

/// Spatial relationship for a join predicate.
#[derive(Clone, Debug)]
pub enum SpatialRelation {
    /// Within a distance.
    Near(f64),
    /// Between two distances.
    Between(f64, f64),
    /// Geometries intersect.
    Intersects,
    /// The outer value contains the inner value, with a common interior.
    Contains,
    /// The outer value covers the inner value, including its boundary.
    Covers,
    /// The outer value is covered by the inner value.
    CoveredBy,
}

/// A node in the execution plan tree.
pub enum PlanNode {
    /// A tantivy query evaluated to per-segment bitsets.
    Query(Box<dyn Query>),

    /// Find K nearest documents to a query geometry.
    Knn {
        /// The spatial field.
        field: Field,
        /// The GeoJSON query geometry.
        geometry: Value,
        /// Number of nearest neighbors.
        k: usize,
        /// If present, only documents passing this node are considered.
        filter: Option<Box<PlanNode>>,
    },

    /// Intersects query filtered by a terms bitset during traversal.
    Intersects {
        /// The spatial field.
        field: Field,
        /// The GeoJSON query geometry in source longitude and latitude.
        geometry: Value,
        /// Documents passing this node are eligible candidates.
        filter: Box<PlanNode>,
    },

    /// Distance query filtered by a terms bitset during traversal.
    Within {
        /// The spatial field.
        field: Field,
        /// The GeoJSON query geometry in source longitude and latitude.
        geometry: Value,
        /// Maximum distance in radians.
        radius: f64,
        /// Documents passing this node are eligible candidates.
        filter: Box<PlanNode>,
    },

    /// Spatial join between an outer and inner result set.
    Join {
        /// The spatial field.
        field: Field,
        /// Documents to test.
        outer: Box<PlanNode>,
        /// Documents to test against.
        inner: Box<PlanNode>,
        /// The spatial relationship.
        relation: SpatialRelation,
    },
}

/// The output of evaluating a plan node. Per-segment results keyed by segment ID.
pub struct StageOutput {
    /// Per-segment results.
    pub results: HashMap<SegmentId, SegmentResult>,
}

/// The precomputed results for a single segment.
pub enum SegmentResult {
    /// A bitset of matching doc_ids with a constant score (join, distance, text queries).
    Match(BitSet),
    /// A scored list of (doc_id, distance) pairs sorted by doc_id (kNN).
    Scored(Vec<(DocId, f32)>),
}

impl StageOutput {
    /// Extract the bitset for a segment, building one from scored results if necessary.
    fn bitset_for(&self, segment_id: &SegmentId, max_doc: u32) -> BitSet {
        match self.results.get(segment_id) {
            Some(SegmentResult::Match(bitset)) => bitset.clone(),
            Some(SegmentResult::Scored(results)) => {
                let mut bitset = BitSet::with_max_value(max_doc);
                for &(doc_id, _) in results {
                    bitset.insert(doc_id);
                }
                bitset
            }
            None => BitSet::with_max_value(max_doc),
        }
    }
}

/// Evaluate a plan node recursively, producing per-segment results.
fn evaluate(
    node: &PlanNode,
    searcher: &Searcher,
    segments: &[SegmentReader],
) -> tantivy::Result<StageOutput> {
    match node {
        PlanNode::Query(query) => {
            // Run the tantivy query to completion per segment, collect bitsets.
            let scoring = EnableScoring::disabled_from_searcher(searcher);
            let weight = query.weight(scoring)?;
            let mut results = HashMap::new();
            for reader in segments {
                let mut scorer = weight.scorer(reader, 1.0)?;
                let mut bitset = BitSet::with_max_value(reader.max_doc());
                let mut doc = scorer.doc();
                while doc != TERMINATED {
                    // A deleted inner geometry can make a live outer document match a join.
                    // Exclude it here, before the join runs.
                    if !reader.is_deleted(doc) {
                        bitset.insert(doc);
                    }
                    doc = scorer.advance();
                }
                results.insert(reader.segment_id(), SegmentResult::Match(bitset));
            }
            Ok(StageOutput { results })
        }

        PlanNode::Intersects {
            field,
            geometry,
            filter,
        } => {
            let filter_output = evaluate(filter, searcher, segments)?;
            let implementation = field_index(&searcher.index().schema(), *field)?;
            let query = implementation.prepare(
                GeometryInput::GeoJson(geometry),
                &SpatialPredicate::Intersects,
            )?;
            search_prepared(*field, query.as_ref(), &filter_output, segments)
        }

        PlanNode::Within {
            field,
            geometry,
            radius,
            filter,
        } => {
            let filter_output = evaluate(filter, searcher, segments)?;
            let implementation = field_index(&searcher.index().schema(), *field)?;
            let query = implementation.prepare(
                GeometryInput::GeoJson(geometry),
                &SpatialPredicate::DistanceWithin(*radius),
            )?;
            search_prepared(*field, query.as_ref(), &filter_output, segments)
        }

        PlanNode::Knn { .. } => {
            todo!("kNN via flood fill tightening")
        }

        PlanNode::Join {
            field,
            outer,
            inner,
            relation,
        } => {
            let implementation = field_index(&searcher.index().schema(), *field)?;
            implementation.validate_join(relation)?;
            let inner_output = evaluate(inner, searcher, segments)?;
            let outer_output = evaluate(outer, searcher, segments)?;
            let mut results = HashMap::new();
            for reader in segments {
                let outer_bitset = outer_output.bitset_for(&reader.segment_id(), reader.max_doc());
                let mut result_bitset = BitSet::with_max_value(reader.max_doc());
                if let Some(spatial) = SpatialFieldReader::open(reader, *field)? {
                    // Check the cell format even though outer values come from the document
                    // directory.
                    CellIndexReader::open(spatial.cells_bytes())?;
                    let mut outer_docs = BitSetDocSet::from(outer_bitset);
                    let mut value_heads = Vec::new();
                    let mut doc_id = outer_docs.doc();
                    while doc_id != TERMINATED {
                        value_heads.clear();
                        implementation.value_heads(
                            spatial.edges_bytes(),
                            doc_id,
                            &mut value_heads,
                        )?;
                        for &head in &value_heads {
                            let query = implementation.prepare_join_query(
                                spatial.edges_bytes(),
                                head,
                                relation,
                            )?;
                            for probe_reader in segments {
                                if let Some(probe) = SpatialFieldReader::open(probe_reader, *field)?
                                {
                                    let filter = inner_output.bitset_for(
                                        &probe_reader.segment_id(),
                                        probe_reader.max_doc(),
                                    );
                                    let hits = query.search(
                                        &probe,
                                        probe_reader.max_doc(),
                                        Some(&filter),
                                    )?;
                                    if hits.len() > 0 {
                                        result_bitset.insert(doc_id);
                                        break;
                                    }
                                }
                            }
                            if result_bitset.contains(doc_id) {
                                break;
                            }
                        }
                        doc_id = outer_docs.advance();
                    }
                }
                results.insert(reader.segment_id(), SegmentResult::Match(result_bitset));
            }
            Ok(StageOutput { results })
        }
    }
}

fn search_prepared(
    field: Field,
    query: &dyn PreparedSpatialQuery,
    filters: &StageOutput,
    segments: &[SegmentReader],
) -> tantivy::Result<StageOutput> {
    let mut results = HashMap::new();
    for reader in segments {
        let hits = if let Some(spatial) = SpatialFieldReader::open(reader, field)? {
            let filter = filters.bitset_for(&reader.segment_id(), reader.max_doc());
            query.search(&spatial, reader.max_doc(), Some(&filter))?
        } else {
            BitSet::with_max_value(reader.max_doc())
        };
        results.insert(reader.segment_id(), SegmentResult::Match(hits));
    }
    Ok(StageOutput { results })
}

/// Query that evaluates a spatial execution plan during Weight construction.
pub struct SpatialExecutor {
    /// The root of the plan tree.
    root: PlanNode,
}

impl SpatialExecutor {
    /// Create a new executor from a plan tree.
    pub fn new(root: PlanNode) -> Self {
        Self { root }
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &PlanNode {
        &self.root
    }

    /// Replace the outer PlanNode in the root Join with the given query.
    pub fn set_outer(&mut self, query: Box<dyn Query>) {
        if let PlanNode::Join { ref mut outer, .. } = self.root {
            *outer = Box::new(PlanNode::Query(query));
        }
    }
}

impl fmt::Debug for SpatialExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpatialExecutor")
    }
}

impl Clone for PlanNode {
    fn clone(&self) -> Self {
        match self {
            PlanNode::Query(q) => PlanNode::Query(q.box_clone()),
            PlanNode::Knn {
                field,
                geometry,
                k,
                filter,
            } => PlanNode::Knn {
                field: *field,
                geometry: geometry.clone(),
                k: *k,
                filter: filter.clone(),
            },
            PlanNode::Intersects {
                field,
                geometry,
                filter,
            } => PlanNode::Intersects {
                field: *field,
                geometry: geometry.clone(),
                filter: filter.clone(),
            },
            PlanNode::Within {
                field,
                geometry,
                radius,
                filter,
            } => PlanNode::Within {
                field: *field,
                geometry: geometry.clone(),
                radius: *radius,
                filter: filter.clone(),
            },
            PlanNode::Join {
                field,
                outer,
                inner,
                relation,
            } => PlanNode::Join {
                field: *field,
                outer: outer.clone(),
                inner: inner.clone(),
                relation: relation.clone(),
            },
        }
    }
}

impl Clone for SpatialExecutor {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
        }
    }
}

impl Query for SpatialExecutor {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let searcher = enable_scoring
            .searcher()
            .expect("SpatialExecutor requires a Searcher");
        let segments = searcher.segment_readers();

        let output = evaluate(&self.root, searcher, segments)?;

        Ok(Box::new(ExecutorWeight {
            results: output.results,
        }))
    }
}

/// Holds precomputed per-segment results. Scorers iterate them.
struct ExecutorWeight {
    /// Precomputed results keyed by segment ID.
    results: HashMap<SegmentId, SegmentResult>,
}

impl Weight for ExecutorWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        match self.results.get(&reader.segment_id()) {
            Some(SegmentResult::Match(bitset)) => {
                Ok(Box::new(ReplayScorer::from_bitset(bitset.clone(), boost)))
            }
            Some(SegmentResult::Scored(results)) => {
                Ok(Box::new(ReplayScorer::from_scored(results.clone())))
            }
            None => Ok(Box::new(ReplayScorer::empty())),
        }
    }

    fn explain(&self, _reader: &SegmentReader, _doc: DocId) -> tantivy::Result<Explanation> {
        Ok(Explanation::new("SpatialExecutor", 1.0))
    }
}

/// Iterates precomputed results as a scorer.
struct ReplayScorer {
    /// The iteration state.
    inner: ReplayInner,
}

enum ReplayInner {
    /// Iterate a bitset with a constant score.
    BitSet {
        docs: BitSetDocSet,
        doc_id: DocId,
        score: Score,
    },
    /// Iterate a sorted vec of (doc_id, distance).
    Scored {
        results: Vec<(DocId, f32)>,
        index: usize,
    },
    /// No results for this segment.
    Empty,
}

impl ReplayScorer {
    fn from_bitset(bitset: BitSet, score: Score) -> Self {
        let docs = BitSetDocSet::from(bitset);
        let doc_id = docs.doc();
        Self {
            inner: ReplayInner::BitSet {
                docs,
                doc_id,
                score,
            },
        }
    }

    fn from_scored(results: Vec<(DocId, f32)>) -> Self {
        Self {
            inner: ReplayInner::Scored { results, index: 0 },
        }
    }

    fn empty() -> Self {
        Self {
            inner: ReplayInner::Empty,
        }
    }
}

impl Scorer for ReplayScorer {
    fn score(&mut self) -> Score {
        match &self.inner {
            ReplayInner::BitSet { score, .. } => *score,
            ReplayInner::Scored { results, index, .. } => {
                if *index < results.len() {
                    -results[*index].1
                } else {
                    0.0
                }
            }
            ReplayInner::Empty => 0.0,
        }
    }
}

impl DocSet for ReplayScorer {
    fn advance(&mut self) -> DocId {
        match &mut self.inner {
            ReplayInner::BitSet { docs, doc_id, .. } => {
                if *doc_id == TERMINATED {
                    return TERMINATED;
                }
                *doc_id = docs.advance();
                *doc_id
            }
            ReplayInner::Scored { results, index, .. } => {
                *index += 1;
                if *index < results.len() {
                    results[*index].0
                } else {
                    TERMINATED
                }
            }
            ReplayInner::Empty => TERMINATED,
        }
    }

    fn doc(&self) -> DocId {
        match &self.inner {
            ReplayInner::BitSet { doc_id, .. } => *doc_id,
            ReplayInner::Scored { results, index, .. } => {
                if *index < results.len() {
                    results[*index].0
                } else {
                    TERMINATED
                }
            }
            ReplayInner::Empty => TERMINATED,
        }
    }

    fn size_hint(&self) -> u32 {
        match &self.inner {
            ReplayInner::BitSet { docs, .. } => docs.size_hint(),
            ReplayInner::Scored { results, .. } => results.len() as u32,
            ReplayInner::Empty => 0,
        }
    }
}
