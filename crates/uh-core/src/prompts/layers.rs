//! The static system-prompt layer: `core.md`, the harness description, the
//! full programmatic-tool API surface (one tool: `ipython`; everything else
//! lives in the REPL), and the usage rules. It leads the cache-stable prefix
//! of the prompt.
//!
//! The layer file must never contain session-specific values: the cached
//! prefix ends where the dynamic tail (`system_prompt.rs`) begins, and the
//! cache-safety guard test pins that boundary.

/// The core harness layer (file `layers/core.md`).
pub const CORE_LAYER: &str = include_str!("layers/core.md");

/// Source file of a layer (breakdown provenance).
#[must_use]
pub fn layer_source(name: &str) -> Option<&'static str> {
    match name {
        "core" => Some("prompts/layers/core.md"),
        _ => None,
    }
}

/// The layered static prefix: the core layer. The model's instructions
/// (settings `modelInstructions`) follow it inside the cached prefix.
#[must_use]
pub fn static_prefix() -> String {
    CORE_LAYER.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_prefix_is_layer_composition() {
        let prefix = static_prefix();
        assert!(prefix.starts_with("# Unharnessed"));
        assert!(prefix.contains("## Rules"));
        assert_eq!(prefix, CORE_LAYER.trim());
    }
}
