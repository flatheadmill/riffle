use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tantivy_spatial::edge_crosser::EdgeCrosser;
use tantivy_spatial::plane::math::orient2d;
use tantivy_spatial::plane::Plane;
use tantivy_spatial::sphere::Sphere;
use tantivy_spatial::surface::Surface;

use super::case::{
    AmbiguousReason, Case, Family, Geometry, InvalidReason, Location, Point, Predicate, Reason,
    Rejection, SurfaceKind, UnsupportedReason,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub matches_without_filter: bool,
    pub selected: bool,
    pub crossings_by_query_member: Vec<usize>,
    pub indexed_vertex_in_query_member: Vec<bool>,
    pub query_member_vertex_in_indexed: Vec<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covered_by_query_member: Vec<Vec<bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub version: String,
    pub expected: Vec<String>,
    pub work: BTreeMap<String, Work>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Difference {
    pub missing: Vec<String>,
    pub spurious: Vec<String>,
}

pub fn compare(expected: &[String], actual: &[String]) -> Difference {
    let expected: BTreeSet<_> = expected.iter().cloned().collect();
    let actual: BTreeSet<_> = actual.iter().cloned().collect();
    Difference {
        missing: expected.difference(&actual).cloned().collect(),
        spurious: actual.difference(&expected).cloned().collect(),
    }
}

struct Member<S: Surface> {
    closed: bool,
    points: Vec<S::Point>,
    uv: Vec<Point>,
}

fn ambiguous(location: Location, family: Family, reason: AmbiguousReason) -> Rejection {
    Rejection {
        location,
        family: Some(family),
        reason: Reason::Ambiguous { reason },
    }
}

fn crosses(a: Point, b: Point, c: Point, d: Point) -> bool {
    let x = orient2d(&a, &b, &c);
    let y = orient2d(&a, &b, &d);
    let z = orient2d(&c, &d, &a);
    let w = orient2d(&c, &d, &b);
    ((x > 0.0 && y < 0.0) || (x < 0.0 && y > 0.0)) && ((z > 0.0 && w < 0.0) || (z < 0.0 && w > 0.0))
}

fn distance_to_edge(p: Point, a: Point, b: Point) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let px = p[0] - a[0];
    let py = p[1] - a[1];
    let t = ((px * dx + py * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - t * dx).hypot(py - t * dy)
}

// Pairwise clearance has already excluded points on or near the ring.
fn inside(point: Point, ring: &[Point]) -> bool {
    let mut inside = false;
    for edge in ring.windows(2) {
        let (a, b) = (edge[0], edge[1]);
        if (a[1] > point[1]) != (b[1] > point[1]) {
            let x = a[0] + (point[1] - a[1]) * (b[0] - a[0]) / (b[1] - a[1]);
            if x > point[0] {
                inside = !inside;
            }
        }
    }
    inside
}

fn validate_ring(
    uv: &[Point],
    location: &Location,
    family: Family,
    margin: f64,
) -> Result<(), Rejection> {
    let invalid = |at, reason| Rejection::invalid(at, Some(family), reason);
    let uncertain = |at, reason| ambiguous(at, family, reason);
    let n = uv.len() - 1;
    for (i, edge) in uv.windows(2).enumerate() {
        if edge[0] == edge[1] {
            return Err(invalid(
                location.at(&format!("/{i}")),
                InvalidReason::ZeroEdge,
            ));
        }
    }
    if uv[1..n].iter().all(|p| orient2d(&uv[0], &uv[1], p) == 0.0) {
        return Err(invalid(location.clone(), InvalidReason::CollapsedRing));
    }
    for i in 0..n {
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            if crosses(uv[i], uv[i + 1], uv[j], uv[j + 1]) {
                return Err(invalid(
                    location.at(&format!("/{i}")),
                    InvalidReason::SelfIntersection,
                ));
            }
        }
    }
    for (i, edge) in uv.windows(2).enumerate() {
        if (edge[0][0] - edge[1][0]).hypot(edge[0][1] - edge[1][1]) <= margin {
            return Err(uncertain(
                location.at(&format!("/{i}")),
                AmbiguousReason::ShortEdge,
            ));
        }
    }
    for i in 0..n {
        if orient2d(&uv[(i + n - 1) % n], &uv[i], &uv[(i + 1) % n]) == 0.0 {
            return Err(uncertain(
                location.at(&format!("/{i}")),
                AmbiguousReason::CollinearVertex,
            ));
        }
        for j in i + 1..n {
            if j == i + 1 || (i == 0 && j == n - 1) {
                continue;
            }
            let (a, b, c, d) = (uv[i], uv[i + 1], uv[j], uv[j + 1]);
            if distance_to_edge(a, c, d) <= margin
                || distance_to_edge(b, c, d) <= margin
                || distance_to_edge(c, a, b) <= margin
                || distance_to_edge(d, a, b) <= margin
            {
                return Err(uncertain(
                    location.at(&format!("/{i}")),
                    AmbiguousReason::RingClearance,
                ));
            }
        }
    }
    Ok(())
}

fn project<S: Surface>(
    geometry: &Geometry,
    location: &Location,
    margin: f64,
) -> Result<Vec<Member<S>>, Rejection> {
    let raw: Vec<(&[Point], bool, Location)> = match geometry {
        Geometry::Point { coordinates } => vec![(
            std::slice::from_ref(coordinates),
            false,
            location.at("/coordinates"),
        )],
        Geometry::Polygon { coordinates } => {
            vec![(&coordinates[0], true, location.at("/coordinates/0"))]
        }
        Geometry::MultiPolygon { coordinates } => coordinates
            .iter()
            .enumerate()
            .map(|(i, rings)| {
                (
                    rings[0].as_slice(),
                    true,
                    location.at(&format!("/coordinates/{i}/0")),
                )
            })
            .collect(),
        _ => {
            return Err(Rejection::unsupported(
                location.clone(),
                Some(geometry.family()),
                UnsupportedReason::Family,
            ));
        }
    };
    let family = geometry.family();
    let mut members = Vec::with_capacity(raw.len());
    for (positions, closed, at) in raw {
        let mut points = Vec::with_capacity(positions.len());
        let mut uv = Vec::with_capacity(positions.len());
        for (i, &[lon, lat]) in positions.iter().enumerate() {
            let vertex = if closed {
                at.at(&format!("/{i}"))
            } else {
                at.clone()
            };
            let p = S::project(lon, lat);
            if S::get_face(&p) != 0 {
                return Err(Rejection::unsupported(
                    vertex,
                    Some(family),
                    UnsupportedReason::Face,
                ));
            }
            let (u, v) = S::point_to_face_uv(0, &p);
            if !u.is_finite() || !v.is_finite() || u.abs() >= 0.95 || v.abs() >= 0.95 {
                return Err(Rejection::unsupported(
                    vertex,
                    Some(family),
                    UnsupportedReason::Face,
                ));
            }
            points.push(p);
            uv.push([u, v]);
        }
        if closed {
            validate_ring(&uv, &at, family, margin)?;
        }
        members.push(Member { closed, points, uv });
    }
    Ok(members)
}

fn boundary_incidence<S: Surface>(a: &[Member<S>], b: &[Member<S>], margin: f64) -> bool {
    for am in a {
        for bm in b {
            for &p in &am.uv {
                if bm
                    .uv
                    .windows(2)
                    .any(|e| distance_to_edge(p, e[0], e[1]) <= margin)
                    || bm
                        .uv
                        .iter()
                        .any(|q| (p[0] - q[0]).hypot(p[1] - q[1]) <= margin)
                {
                    return true;
                }
            }
            for &p in &bm.uv {
                if am
                    .uv
                    .windows(2)
                    .any(|e| distance_to_edge(p, e[0], e[1]) <= margin)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn topology<S: Surface>(
    indexed: &[Member<S>],
    query: &[Member<S>],
    selected: bool,
    predicate: Predicate,
) -> Work {
    let mut crossings = vec![0; query.len()];
    let mut indexed_inside = vec![false; query.len()];
    let mut query_inside = vec![false; query.len()];
    let mut covered = Vec::new();
    for a in indexed {
        let mut member_covered = Vec::new();
        for (j, b) in query.iter().enumerate() {
            let mut pair_crossings = 0;
            for ae in a.points.windows(2) {
                let mut crosser = S::EdgeCrosser::new(&ae[0], &ae[1]);
                for be in b.points.windows(2) {
                    if crosser.crossing_sign_two(&be[0], &be[1]) > 0 {
                        pair_crossings += 1;
                    }
                }
            }
            crossings[j] += pair_crossings;
            if b.closed {
                indexed_inside[j] |= a.uv.iter().any(|&p| inside(p, &b.uv));
            }
            if a.closed {
                query_inside[j] |= b.uv.iter().any(|&p| inside(p, &a.uv));
            }
            if predicate == Predicate::CoveredBy {
                member_covered.push(pair_crossings == 0 && inside(a.uv[0], &b.uv));
            }
        }
        if predicate == Predicate::CoveredBy {
            covered.push(member_covered);
        }
    }
    let matches_without_filter = match predicate {
        Predicate::Intersects => {
            crossings.iter().any(|&n| n > 0)
                || indexed_inside.iter().any(|&yes| yes)
                || query_inside.iter().any(|&yes| yes)
        }
        Predicate::CoveredBy => covered.iter().all(|member| member.iter().any(|&yes| yes)),
    };
    Work {
        matches_without_filter,
        selected,
        crossings_by_query_member: crossings,
        indexed_vertex_in_query_member: indexed_inside,
        query_member_vertex_in_indexed: query_inside,
        covered_by_query_member: covered,
    }
}

fn evaluate_surface<S: Surface>(case: &Case) -> Result<Answer, Rejection> {
    let query = project::<S>(
        &case.query,
        &Location::Query {
            path: String::new(),
        },
        case.margin,
    )?;
    if case.predicate == Predicate::CoveredBy {
        for i in 0..query.len() {
            for j in i + 1..query.len() {
                let a = std::slice::from_ref(&query[i]);
                let b = std::slice::from_ref(&query[j]);
                let at = Location::Query {
                    path: format!("/coordinates/{j}"),
                };
                if boundary_incidence(a, b, case.margin) {
                    return Err(ambiguous(
                        at,
                        case.query.family(),
                        AmbiguousReason::BoundaryIncidence,
                    ));
                }
                if topology(a, b, false, Predicate::Intersects).matches_without_filter {
                    return Err(Rejection::unsupported(
                        at,
                        Some(case.query.family()),
                        UnsupportedReason::QueryMembersOverlap,
                    ));
                }
            }
        }
    }
    let mut expected = Vec::new();
    let mut work = BTreeMap::new();
    for doc in &case.documents {
        let at = Location::Document {
            id: doc.id.clone(),
            path: String::new(),
        };
        let geometry = project::<S>(&doc.geometry, &at, case.margin)?;
        // Excluded documents can still affect index subdivision and failures.
        if boundary_incidence(&geometry, &query, case.margin) {
            return Err(ambiguous(
                at,
                doc.geometry.family(),
                AmbiguousReason::BoundaryIncidence,
            ));
        }
        let document_work = topology(
            &geometry,
            &query,
            case.filter.selected(&doc.id),
            case.predicate,
        );
        if document_work.matches_without_filter && document_work.selected {
            expected.push(doc.id.clone());
        }
        work.insert(doc.id.clone(), document_work);
    }
    expected.sort();
    Ok(Answer {
        version: case.predicate.longhand_version().into(),
        expected,
        work,
    })
}

pub fn evaluate(case: &Case) -> Result<Answer, Rejection> {
    case.validate_structure()?;
    case.validate_supported()?;
    match case.surface {
        SurfaceKind::Plane => evaluate_surface::<Plane>(case),
        SurfaceKind::Sphere => evaluate_surface::<Sphere>(case),
    }
}
