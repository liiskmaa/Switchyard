use serde::{Deserialize, Serialize};
use serde_json::Value;
use switchyard_events::EventEnvelope;
use uuid::Uuid;

use crate::error::ProtocolError;

/// Client-chosen request identifier, echoed in the matching response.
pub type RequestId = u64;

/// Frames sent from a client to Core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Request {
        id: RequestId,
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// Frames sent from Core to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Successful result of request `id`.
    Response { id: RequestId, result: Value },
    /// Failed request. `id` is absent when the failing frame could not be
    /// attributed to a request (e.g. unparseable input).
    Error {
        #[serde(default)]
        id: Option<RequestId>,
        error: ProtocolError,
    },
    /// An event delivered on an active subscription.
    Event { subscription_id: Uuid, event: EventEnvelope },
    /// Core ended a subscription on its own (e.g. a journal read failed).
    /// The client may resubscribe with `after_seq` set to the last seen seq.
    SubscriptionClosed { subscription_id: Uuid, error: ProtocolError },
    /// Core is shutting down; the connection will close after this frame.
    Shutdown { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn request_wire_shape() {
        let frame: ClientFrame =
            serde_json::from_str(r#"{"type":"request","id":3,"method":"system.health"}"#).unwrap();
        assert_eq!(
            frame,
            ClientFrame::Request { id: 3, method: "system.health".into(), params: Value::Null }
        );
    }

    #[test]
    fn error_wire_shape() {
        let frame =
            ServerFrame::Error { id: Some(9), error: ProtocolError::new(ErrorCode::NotFound, "nope") };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type":"error","id":9,"error":{"code":"not_found","message":"nope"}})
        );
        assert_eq!(serde_json::from_value::<ServerFrame>(json).unwrap(), frame);
    }
}
