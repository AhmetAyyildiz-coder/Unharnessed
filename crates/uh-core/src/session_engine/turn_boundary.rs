//! The turn-boundary host-request surface: `model.info`, which the kernel
//! reaches through `rlm.host_request`. Port of the `model.info` handler of
//! core/agent-session.ts.

use std::sync::Arc;

use serde_json::json;

use crate::kernel::shared::{host_handler, HostRequestHandlers};

/// The model facts `model.info` reports (TS answers nulls when the session
/// has no model; the Rust engine always resolves one).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    /// Input modalities (empty when the resolved model did not declare
    /// any, e.g. harness-converted minimal models).
    pub input: Vec<uh_types::ai::ModelInput>,
}

/// The runtime the handler reads once the session is assembled: the model
/// facts `model.info` reports. The handler is registered before the
/// session exists; `create_session` binds this before returning.
pub struct TurnBoundaryRuntime {
    pub model_info: ModelInfo,
}

/// The turn-boundary state: the late-bound runtime. Build it in `Arc` form
/// so the registered host handler observes the same cell.
#[derive(Default)]
pub struct TurnBoundaryRequests {
    /// The late-bound runtime; `bind` is first-wins (the session assembles
    /// once), `rebind_model_facts` swaps the model facts after a live
    /// model switch (the registered handler reads through `bound()` on
    /// every request, so it follows the model the session now runs).
    runtime: std::sync::RwLock<Option<Arc<TurnBoundaryRuntime>>>,
}

impl TurnBoundaryRequests {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the assembled session runtime (first bind wins).
    pub fn bind(&self, runtime: TurnBoundaryRuntime) {
        let mut cell = self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cell.is_none() {
            *cell = Some(Arc::new(runtime));
        }
    }

    /// The bound runtime (`None` until the session is assembled).
    pub fn bound(&self) -> Option<Arc<TurnBoundaryRuntime>> {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Re-bind the runtime's model facts after a live model switch
    /// (`model.info` follows the new model — the TS runtime reads it from
    /// the model the session runs, not the assembly-time one).
    pub fn rebind_model_facts(&self, model_info: ModelInfo) {
        let mut cell = self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cell.is_some() {
            *cell = Some(Arc::new(TurnBoundaryRuntime { model_info }));
        }
    }

    /// Register `model.info` (always present, like the TS `_hostHandlers`
    /// default map).
    pub fn register_model_info_handler(
        self: &Arc<Self>,
        handlers: &mut HostRequestHandlers,
        model_info: ModelInfo,
    ) {
        // Weak, upgraded at request time: the kernel holds these handlers
        // for its whole life and its host graph reaches the session, so a
        // strong capture here loops the ownership graph and pins a dropped
        // session's kernel process until the process exits. The engine owns
        // this requests object (see SessionEngine::turn_boundary).
        let requests = Arc::downgrade(self);
        handlers.register(
            "model.info",
            host_handler(move |_payload| {
                let requests = requests.clone();
                let model_info = model_info.clone();
                Box::pin(async move {
                    // The bound runtime is authoritative once the session
                    // is live; the registration-time facts cover pre-bind
                    // probes AND a dropped session (model.info always
                    // answers, like the TS default map).
                    let model_info = requests
                        .upgrade()
                        .and_then(|requests| {
                            requests.bound().map(|runtime| runtime.model_info.clone())
                        })
                        .unwrap_or(model_info);
                    Ok(json!({
                        "id": model_info.id,
                        "provider": model_info.provider,
                        "input": model_info.input.iter().map(|input| match input {
                            uh_types::ai::ModelInput::Text => "text",
                            uh_types::ai::ModelInput::Image => "image",
                        }).collect::<Vec<&str>>(),
                    }))
                })
            }),
        );
    }
}

// The unit battery lives in the child module (turn_boundary::tests); its
// use-super glob resolves through this facade's bindings and re-exports.
#[cfg(test)]
mod tests;
