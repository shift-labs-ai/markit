//! Shared Open Packaging Convention path and root-relationship helpers.
//!
//! OOXML main parts are relationship-addressed; conventional filenames are
//! defaults, not fixed locations.

use std::collections::HashMap;

use anyhow::{anyhow, Result};

pub(crate) type RelationshipMap = HashMap<String, (String, String)>;

pub(crate) fn parse_relationships(xml: &str) -> RelationshipMap {
    let mut relationships = HashMap::new();
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return relationships;
    };
    for relationship in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Relationship")
    {
        let (Some(id), Some(target)) = (
            relationship.attribute("Id"),
            relationship.attribute("Target"),
        ) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        relationships.insert(
            id.to_string(),
            (
                relationship.attribute("Type").unwrap_or("").to_string(),
                target.to_string(),
            ),
        );
    }
    relationships
}

pub(crate) fn office_document_path(root_relationships_xml: &str) -> Result<String> {
    let relationships = parse_relationships(root_relationships_xml);
    let target = relationships
        .iter()
        .filter(|(_, (kind, _))| kind.ends_with("/officeDocument"))
        .min_by_key(|(id, _)| id.as_str())
        .map(|(_, (_, target))| target)
        .ok_or_else(|| anyhow!("package has no officeDocument relationship"))?;
    resolve_part_path("", target).ok_or_else(|| anyhow!("invalid officeDocument target"))
}

pub(crate) fn relationship_part_path(source_part: &str) -> String {
    match source_part.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/_rels/{name}.rels"),
        None => format!("_rels/{source_part}.rels"),
    }
}

pub(crate) fn resolve_part_path(source_part: &str, target: &str) -> Option<String> {
    let target = target.replace('\\', "/");
    let mut components: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        source_part
            .rsplit_once('/')
            .map(|(directory, _)| directory.split('/').collect())
            .unwrap_or_default()
    };
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_absolute_and_parent_targets() {
        assert_eq!(
            resolve_part_path("deck/pres.xml", "s/one.xml").as_deref(),
            Some("deck/s/one.xml")
        );
        assert_eq!(
            resolve_part_path("ppt/slides/one.xml", "../media/a.png").as_deref(),
            Some("ppt/media/a.png")
        );
        assert_eq!(
            resolve_part_path("word/document.xml", "/custom/main.xml").as_deref(),
            Some("custom/main.xml")
        );
        assert_eq!(
            resolve_part_path("word/document.xml", "../../escape.xml"),
            None
        );
    }

    #[test]
    fn recognizes_transitional_and_strict_root_relationships() {
        for kind in [
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument",
            "http://purl.oclc.org/ooxml/officeDocument/relationships/officeDocument",
        ] {
            let xml = format!(
                r#"<Relationships><Relationship Id="rId1" Type="{kind}" Target="custom/main.xml"/></Relationships>"#
            );
            assert_eq!(office_document_path(&xml).unwrap(), "custom/main.xml");
        }
    }
}
