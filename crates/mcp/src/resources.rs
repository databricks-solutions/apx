use crate::info_content::APX_INFO_CONTENT;
use rmcp::model::*;

pub fn list_resources() -> Vec<Resource> {
    let mut raw = RawResource::new("apx://info", "apx-info".to_string());
    raw.description = Some("Information about apx toolkit".to_string());
    raw.mime_type = Some("text/plain".to_string());
    vec![raw.no_annotation()]
}

pub fn read_resource(uri: &str) -> Result<ReadResourceResult, String> {
    match uri {
        "apx://info" => Ok(ReadResourceResult {
            contents: vec![ResourceContents::text(APX_INFO_CONTENT, uri)],
        }),
        _ => Err(format!("Resource not found: {uri}")),
    }
}
