//! System-prompt assembly for the RLM harness. The prompt is layered:
//!
//! - the **cached static prefix** is the human-editable layer file
//!   ([`layers`]: the harness description, its tool surface and rules),
//!   then the instructions the settings give the session's model
//!   (`modelInstructions`). It never varies between sessions on the same
//!   model and settings, so providers can cache it.
//! - the **dynamic tail** carries everything session-specific (packages,
//!   project context, skills inventory, MCP servers, environment, role) and
//!   is appended after the static prefix, in that order.
//!
//! `system_prompt_breakdown` exposes the per-layer segments so the CLI can
//! dump exactly what the model sees; the cache-safety and tool-surface guard
//! tests pin the boundary and the documented API surface.

pub mod layers;

pub mod system_prompt;

pub use system_prompt::{
    build_system_prompt, system_prompt_breakdown, BuildSystemPromptOptions, PromptSegment,
    SegmentKind, SystemPromptBreakdown,
};
