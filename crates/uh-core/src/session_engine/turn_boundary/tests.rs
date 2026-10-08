//! The turn-boundary unit battery: the model-info reports.
use super::*;
use crate::kernel::shared::{HostRequestHandlers, HostRequestPayload};
use serde_json::Value;

fn payload(data: Value) -> HostRequestPayload {
    HostRequestPayload {
        data,
        cell_source_code: None,
    }
}

fn model_info() -> ModelInfo {
    ModelInfo {
        id: "faux-1".to_string(),
        provider: "faux".to_string(),
        input: vec![uh_types::ai::ModelInput::Text],
    }
}

fn registered(requests: &Arc<TurnBoundaryRequests>) -> HostRequestHandlers {
    let mut handlers = HostRequestHandlers::default();
    requests.register_model_info_handler(&mut handlers, model_info());
    handlers
}

fn handler(
    handlers: &HostRequestHandlers,
    request_type: &str,
) -> crate::kernel::shared::HostHandlerFn {
    handlers.get(request_type).expect("registered").clone()
}

#[tokio::test]
async fn model_info_reports_the_bound_model() {
    let requests = Arc::new(TurnBoundaryRequests::new());
    let handlers = registered(&requests);
    // Before the runtime binds, the registration-time facts answer.
    let response = handler(&handlers, "model.info")(payload(json!({})))
        .await
        .unwrap();
    assert_eq!(response["id"], "faux-1");
    assert_eq!(response["provider"], "faux");
    assert_eq!(response["input"], json!(["text"]));
    // A bound runtime stays authoritative.
    requests.bind(TurnBoundaryRuntime {
        model_info: ModelInfo {
            id: "other-1".to_string(),
            provider: "other".to_string(),
            input: Vec::new(),
        },
    });
    let response = handler(&handlers, "model.info")(payload(json!({})))
        .await
        .unwrap();
    assert_eq!(response["id"], "other-1");
    assert_eq!(response["provider"], "other");
    assert_eq!(response["input"], json!([]));
}
