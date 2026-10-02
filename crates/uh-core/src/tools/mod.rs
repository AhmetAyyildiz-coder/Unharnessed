//! Tool implementations ported from `packages/coding-agent/src/core/tools/`.
//!
//! All modules are `pub(crate)` internals of the tools subsystem; the
//! crate facade (crate root) re-exports only the public tool surface.

// Rendering preview helpers (TS code-preview.ts).
pub(crate) mod code_preview;
pub(crate) mod code_preview_python;
pub(crate) mod golden_replay;
pub(crate) mod ipython;
pub(crate) mod ipython_cell_code;
pub(crate) mod path_utils;
pub(crate) mod tool_definition;
