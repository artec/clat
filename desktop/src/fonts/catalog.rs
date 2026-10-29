//! Device font metadata only: no paths, font bytes or Host state are exposed.
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    family: String,
    full_name: String,
    postscript: String,
    monospace: bool,
}
pub(super) fn inventory() -> Result<Vec<Face>, String> {
    let handles = font_kit::source::SystemSource::new()
        .all_fonts()
        .map_err(|e| e.to_string())?;
    let mut faces = Vec::new();
    for handle in handles {
        let Ok(font) = handle.load() else { continue };
        let Some(postscript) = font.postscript_name() else {
            continue;
        };
        faces.push(Face {
            family: font.family_name(),
            full_name: font.full_name(),
            postscript,
            monospace: font.is_monospace(),
        });
    }
    faces.sort_by(|a, b| (&a.family, &a.full_name).cmp(&(&b.family, &b.full_name)));
    faces.dedup_by(|a, b| a.postscript == b.postscript);
    Ok(faces)
}
#[cfg(test)]
mod tests {
    #[test]
    fn native_font_catalog_contains_addressable_faces() {
        let rows = super::inventory().expect("native font catalog");
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .all(|r| !r.family.is_empty() && !r.postscript.is_empty())
        );
        let family = if cfg!(target_os = "macos") {
            "Menlo"
        } else {
            "Consolas"
        };
        assert!(rows.iter().any(|r| r.family == family));
    }
}
