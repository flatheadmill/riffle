use query_grammar::{Delimiter, SpatialPredicateKind, UserInputAst, UserInputBound, UserInputLeaf};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, Occur, Query,
};
use tantivy::query_grammar as ordinary;
use tantivy::schema::{Field, FieldType, Schema};
use tantivy::tokenizer::TokenizerManager;
use tantivy::{Index, Score};

use super::{QueryParserError, SpatialPredicate, SpatialQuery};

/// Resolves Riffle query syntax against an index schema.
pub struct QueryParser {
    schema: Schema,
    ordinary: tantivy::query::QueryParser,
    conjunction_by_default: bool,
}

enum Node {
    Omitted,
    Query(Box<dyn Query>),
    Clause(Vec<(Occur, Node)>),
    Boost(Box<Node>, Score),
}

impl Node {
    fn all_negative(&self) -> bool {
        match self {
            Self::Omitted => true,
            Self::Query(_) => false,
            Self::Boost(child, _) => child.all_negative(),
            Self::Clause(children) => children
                .iter()
                .all(|(occur, child)| *occur == Occur::MustNot || child.all_negative()),
        }
    }

    fn make_non_negative(&mut self) {
        match self {
            Self::Omitted => {
                *self = Self::Clause(vec![(Occur::Should, Self::Query(Box::new(AllQuery)))]);
            }
            Self::Clause(children) => {
                children.push((Occur::Should, Self::Query(Box::new(AllQuery))))
            }
            Self::Boost(child, _) => child.make_non_negative(),
            Self::Query(_) => {}
        }
    }

    fn simplify(self) -> Self {
        let Self::Clause(children) = self else {
            return self;
        };
        let mut simplified = Vec::new();
        for (occur, child) in children {
            match child.simplify() {
                Self::Omitted if occur != Occur::MustNot => {}
                Self::Clause(children)
                    if occur != Occur::MustNot
                        && children.iter().all(|(other, _)| *other == occur) =>
                {
                    simplified.extend(children);
                }
                child => simplified.push((occur, child)),
            }
        }
        Self::Clause(simplified)
    }

    fn trim(self) -> Option<Self> {
        match self {
            Self::Omitted => None,
            Self::Clause(children) => {
                let children: Vec<_> = children
                    .into_iter()
                    .filter_map(|(occur, child)| child.trim().map(|child| (occur, child)))
                    .collect();
                (!children.is_empty()).then_some(Self::Clause(children))
            }
            // Boosts preserve an omitted operand until executable conversion.
            node => Some(node),
        }
    }

    fn compile(self) -> Box<dyn Query> {
        match self.trim() {
            None => Box::new(EmptyQuery),
            Some(Self::Query(query)) => query,
            Some(Self::Boost(child, boost)) => Box::new(BoostQuery::new(child.compile(), boost)),
            Some(Self::Clause(children)) => {
                let mut children: Vec<_> = children
                    .into_iter()
                    .map(|(occur, child)| (occur, child.compile()))
                    .collect();
                if children.iter().all(|(occur, _)| *occur == Occur::MustNot) {
                    children.push((
                        Occur::Must,
                        Box::new(ConstScoreQuery::new(Box::new(AllQuery), 0.0)),
                    ));
                }
                Box::new(BooleanQuery::new(children))
            }
            Some(Self::Omitted) => unreachable!("omitted node was trimmed"),
        }
    }
}

impl QueryParser {
    pub fn new(schema: Schema, default_fields: Vec<Field>, tokenizers: TokenizerManager) -> Self {
        Self {
            ordinary: tantivy::query::QueryParser::new(schema.clone(), default_fields, tokenizers),
            schema,
            conjunction_by_default: false,
        }
    }

    pub fn for_index(index: &Index, default_fields: Vec<Field>) -> Self {
        Self::new(index.schema(), default_fields, index.tokenizers().clone())
    }

    pub fn set_conjunction_by_default(&mut self) {
        self.conjunction_by_default = true;
    }

    pub fn set_field_boost(&mut self, field: Field, boost: Score) {
        self.ordinary.set_field_boost(field, boost);
    }

    pub fn set_field_fuzzy(&mut self, field: Field, prefix: bool, distance: u8, transpose: bool) {
        self.ordinary
            .set_field_fuzzy(field, prefix, distance, transpose);
    }

    pub fn allow_regexes(&mut self) {
        self.ordinary.allow_regexes();
    }

    pub fn parse_query(&self, text: &str) -> Result<Box<dyn Query>, QueryParserError> {
        let ast = query_grammar::parse_query(text)
            .map_err(|_| QueryParserError::SyntaxError(text.to_owned()))?;
        let (node, errors) = self.resolve_root(ast);
        first_error(errors)?;
        Ok(node.simplify().compile())
    }

    pub fn parse_query_lenient(&self, text: &str) -> (Box<dyn Query>, Vec<QueryParserError>) {
        let (ast, errors) = query_grammar::parse_query_lenient(text);
        let mut errors: Vec<_> = errors
            .into_iter()
            .map(|error| {
                QueryParserError::SyntaxError(format!(
                    "{} at position {}",
                    error.message, error.pos
                ))
            })
            .collect();
        let (node, mut resolution_errors) = self.resolve_root(ast);
        errors.append(&mut resolution_errors);
        (node.compile(), errors)
    }

    pub fn build_query_from_user_input_ast(
        &self,
        ast: UserInputAst,
    ) -> Result<Box<dyn Query>, QueryParserError> {
        let (node, errors) = self.resolve_root(ast);
        first_error(errors)?;
        Ok(node.compile())
    }

    pub fn build_query_from_user_input_ast_lenient(
        &self,
        ast: UserInputAst,
    ) -> (Box<dyn Query>, Vec<QueryParserError>) {
        let (node, errors) = self.resolve_root(ast);
        (node.compile(), errors)
    }

    fn resolve_root(&self, ast: UserInputAst) -> (Node, Vec<QueryParserError>) {
        let (mut node, mut errors) = self.resolve(ast);
        if !matches!(node, Node::Omitted) && node.all_negative() {
            errors.push(QueryParserError::AllButQueryForbidden);
            node.make_non_negative();
        }
        (node, errors)
    }

    fn resolve(&self, ast: UserInputAst) -> (Node, Vec<QueryParserError>) {
        match ast {
            UserInputAst::Clause(children) if children.is_empty() => {
                (Node::Query(Box::new(EmptyQuery)), Vec::new())
            }
            UserInputAst::Clause(children) => {
                let default = if self.conjunction_by_default {
                    Occur::Must
                } else {
                    Occur::Should
                };
                let mut nodes = Vec::with_capacity(children.len());
                let mut errors = Vec::new();
                for (occur, child) in children {
                    let (node, mut child_errors) = self.resolve(child);
                    nodes.push((occur.map(translate_occur).unwrap_or(default), node));
                    errors.append(&mut child_errors);
                }
                (Node::Clause(nodes), errors)
            }
            UserInputAst::Boost(child, boost) => {
                let (node, errors) = self.resolve(*child);
                let boost = boost.into_inner() as Score;
                let node = if (boost - 1.0).abs() < Score::EPSILON {
                    node
                } else {
                    Node::Boost(Box::new(node), boost)
                };
                (node, errors)
            }
            UserInputAst::Leaf(leaf) => self.resolve_leaf(*leaf),
        }
    }

    fn resolve_leaf(&self, leaf: UserInputLeaf) -> (Node, Vec<QueryParserError>) {
        let leaf = match leaf {
            UserInputLeaf::Literal(value) => {
                ordinary::UserInputLeaf::Literal(ordinary::UserInputLiteral {
                    field_name: value.field_name,
                    phrase: value.phrase,
                    delimiter: match value.delimiter {
                        Delimiter::None => ordinary::Delimiter::None,
                        Delimiter::SingleQuotes => ordinary::Delimiter::SingleQuotes,
                        Delimiter::DoubleQuotes => ordinary::Delimiter::DoubleQuotes,
                    },
                    slop: value.slop,
                    prefix: value.prefix,
                })
            }
            UserInputLeaf::All => ordinary::UserInputLeaf::All,
            UserInputLeaf::Range {
                field,
                lower,
                upper,
            } => ordinary::UserInputLeaf::Range {
                field,
                lower: translate_bound(lower),
                upper: translate_bound(upper),
            },
            UserInputLeaf::Set { field, elements } => {
                ordinary::UserInputLeaf::Set { field, elements }
            }
            UserInputLeaf::Exists { field } => ordinary::UserInputLeaf::Exists { field },
            UserInputLeaf::Regex { field, pattern } => {
                ordinary::UserInputLeaf::Regex { field, pattern }
            }
            UserInputLeaf::Spatial {
                field,
                predicate,
                coordinates,
                inner_query,
            } => {
                return self
                    .resolve_spatial(
                        field,
                        predicate,
                        coordinates
                            .into_iter()
                            .map(|(lon, lat)| [lon.0, lat.0])
                            .collect(),
                        inner_query,
                    )
                    .unwrap_or_else(|error| (Node::Omitted, vec![error]));
            }
        };
        self.ordinary_leaf(leaf)
    }

    fn ordinary_leaf(&self, leaf: ordinary::UserInputLeaf) -> (Node, Vec<QueryParserError>) {
        let (query, errors) = self
            .ordinary
            .build_query_from_user_input_ast_lenient(ordinary::UserInputAst::Leaf(Box::new(leaf)));
        // Only a single ordinary leaf has this omission contract. Empty sets remain
        // TermSetQuery operands; explicit empty ASTs never pass through this adapter.
        let node = if query.is::<EmptyQuery>() {
            Node::Omitted
        } else {
            Node::Query(query)
        };
        (node, errors)
    }

    fn resolve_spatial(
        &self,
        field: Option<String>,
        predicate: SpatialPredicateKind,
        coordinates: Vec<[f64; 2]>,
        inner: Option<Box<UserInputAst>>,
    ) -> Result<(Node, Vec<QueryParserError>), QueryParserError> {
        let path = field.ok_or_else(|| {
            QueryParserError::UnsupportedQuery(
                "Spatial query requires an explicit field name.".into(),
            )
        })?;
        let (field, json_path) = self
            .schema
            .find_field(&path)
            .ok_or_else(|| QueryParserError::FieldDoesNotExist(path.clone()))?;
        if !json_path.is_empty() {
            return Err(QueryParserError::UnsupportedQuery(
                "Spatial query does not support json paths.".into(),
            ));
        }
        if !matches!(self.schema.get_field_entry(field).field_type(), FieldType::Custom(options) if options.type_name() == "spatial")
        {
            return Err(QueryParserError::UnsupportedQuery(
                "Spatial predicates require a spatial field.".into(),
            ));
        }
        if inner.is_some() && !coordinates.is_empty() {
            return Err(QueryParserError::UnsupportedQuery(
                "Spatial joins cannot also specify coordinates.".into(),
            ));
        }
        let predicate = match predicate {
            SpatialPredicateKind::Intersects => SpatialPredicate::Intersects,
            SpatialPredicateKind::Contains => SpatialPredicate::Contains,
            SpatialPredicateKind::Covers => SpatialPredicate::Covers,
            SpatialPredicateKind::CoveredBy => SpatialPredicate::CoveredBy,
            SpatialPredicateKind::Within(radius) => SpatialPredicate::DistanceWithin(radius.0),
            SpatialPredicateKind::Between(_, _) => {
                return Err(QueryParserError::UnsupportedQuery(
                    "$between is not supported.".into(),
                ))
            }
            SpatialPredicateKind::Knn(_) => {
                return Err(QueryParserError::UnsupportedQuery(
                    "$knn is not supported.".into(),
                ))
            }
        };
        if inner.is_some() {
            return Err(QueryParserError::UnsupportedQuery(
                "Spatial joins are not available.".into(),
            ));
        }
        Ok((
            Node::Query(Box::new(SpatialQuery::with_predicate(
                field,
                coordinates,
                predicate,
            ))),
            Vec::new(),
        ))
    }
}

fn first_error(errors: Vec<QueryParserError>) -> Result<(), QueryParserError> {
    errors.into_iter().next().map_or(Ok(()), Err)
}

fn translate_occur(occur: query_grammar::Occur) -> Occur {
    match occur {
        query_grammar::Occur::Must => Occur::Must,
        query_grammar::Occur::Should => Occur::Should,
        query_grammar::Occur::MustNot => Occur::MustNot,
    }
}

fn translate_bound(bound: UserInputBound) -> ordinary::UserInputBound {
    match bound {
        UserInputBound::Unbounded => ordinary::UserInputBound::Unbounded,
        UserInputBound::Inclusive(value) => ordinary::UserInputBound::Inclusive(value),
        UserInputBound::Exclusive(value) => ordinary::UserInputBound::Exclusive(value),
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
