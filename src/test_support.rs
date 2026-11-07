use serde_json::Value;
use tantivy::schema::Schema;
use tantivy::TantivyDocument;

use crate::spatial_input::OpaqueGeoJson;

pub(crate) fn parse_document(schema: &Schema, text: &str) -> tantivy::Result<TantivyDocument> {
    let mut value: Value = serde_json::from_str(text)?;
    let mut spatial = Vec::new();
    for (field, entry) in schema.fields() {
        if OpaqueGeoJson::index_name(entry)?.is_some() {
            if let Some(geometry) = value.as_object_mut().unwrap().remove(entry.name()) {
                let geometries = match geometry {
                    Value::Array(values) => values,
                    geometry => vec![geometry],
                };
                for geometry in geometries {
                    spatial.push((field, serde_json::to_vec(&geometry)?));
                }
            }
        }
    }
    let mut document = TantivyDocument::parse_json(schema, &value.to_string())?;
    for (field, bytes) in spatial {
        document.add_custom(field, &bytes);
    }
    Ok(document)
}
