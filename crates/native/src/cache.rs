use std::fs;
use std::path::Path;
use sumo_parser_core::{KifStore, Span, ParseError};

/// Serialize `store` to a JSON file at `path`.
pub fn save_cache(store: &KifStore, path: &Path) -> Result<(), String> {
    let json =
        serde_json::to_string(store).map_err(|e| format!("failed to serialise cache: {}", e))?;
    fs::write(path, json).map_err(|e| format!("failed to write cache to {}: {}", path.display(), e))
}

/// Deserialise a `KifStore` from a JSON cache file at `path`.
pub fn load_cache(path: &Path) -> Result<KifStore, (Span, ParseError)> {
    let empty_span = Span {
        file: path.to_str().unwrap().to_string(),
        line: 0,
        col: 0,
        offset: 0
    };
    let json = fs::read_to_string(path).map_err(|e| {
        (
            empty_span.clone(),
            ParseError::Other {
                msg: format!("failed to read cache from {}: {}", path.display(), e),
            },
        )
    })?;
    serde_json::from_str(&json).map_err(|e| {
        (
            empty_span.clone(),
            ParseError::Other {
                msg: format!("failed to deserialise cache from {}: {}", path.display(), e),
            },
        )
    })
}
