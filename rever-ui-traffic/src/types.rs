//! Port of `src/renderer/src/types/traffic.ts` — the list's view of a captured
//! request, the endpoint buckets built from it, and the capture events that feed both.
//!
//! Timestamps are `f64` because the events carry whatever the capture layer
//! measured (CDP reports fractional seconds); sizes and statuses are integers.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficEntry {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub resource_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoded_data_length: Option<u64>,
    pub started_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<f64>,
    /// Set only for XHR/Fetch — links this entry to its ApiEndpoint bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    pub key: String,
    pub origin: String,
    pub host: String,
    pub path: String,
    pub method: String,
    pub count: u64,
    pub last_request_id: String,
    pub last_seen: f64,
    pub statuses: Vec<u16>,
    pub auth: bool,
}

/// The field names are snake_case on the wire, unlike the two structs above.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NetworkEvent {
    Request {
        request_id: String,
        url: String,
        method: String,
        resource_type: String,
        timestamp: f64,
    },
    Response {
        request_id: String,
        status: u16,
        mime_type: String,
        timestamp: f64,
    },
    Finished {
        request_id: String,
        encoded_data_length: u64,
        timestamp: f64,
    },
}

impl NetworkEvent {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Request { request_id, .. }
            | Self::Response { request_id, .. }
            | Self::Finished { request_id, .. } => request_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 요청_이벤트는_type_태그와_스네이크_케이스_필드로_읽고_쓴다() {
        let wire = json!({
            "type": "request",
            "request_id": "1.2",
            "url": "https://a.test/x",
            "method": "GET",
            "resource_type": "XHR",
            "timestamp": 12345.678
        });
        let event: NetworkEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            event,
            NetworkEvent::Request {
                request_id: "1.2".into(),
                url: "https://a.test/x".into(),
                method: "GET".into(),
                resource_type: "XHR".into(),
                timestamp: 12345.678,
            }
        );
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
        assert_eq!(event.request_id(), "1.2");
    }

    #[test]
    fn 응답과_완료_이벤트도_같은_형식이다() {
        let response: NetworkEvent = serde_json::from_value(json!({
            "type": "response", "request_id": "r", "status": 404,
            "mime_type": "text/html", "timestamp": 2.5
        }))
        .unwrap();
        assert_eq!(
            response,
            NetworkEvent::Response {
                request_id: "r".into(),
                status: 404,
                mime_type: "text/html".into(),
                timestamp: 2.5,
            }
        );
        let finished: NetworkEvent = serde_json::from_value(json!({
            "type": "finished", "request_id": "r", "encoded_data_length": 2048, "timestamp": 3
        }))
        .unwrap();
        assert_eq!(
            finished,
            NetworkEvent::Finished {
                request_id: "r".into(),
                encoded_data_length: 2048,
                timestamp: 3.0,
            }
        );
        assert_eq!(finished.request_id(), "r");
    }

    #[test]
    fn 모르는_type이나_빠진_필드는_오류로_돌려준다() {
        assert!(serde_json::from_value::<NetworkEvent>(json!({ "type": "nope" })).is_err());
        assert!(
            serde_json::from_value::<NetworkEvent>(
                json!({ "type": "response", "request_id": "r" })
            )
            .is_err()
        );
    }

    #[test]
    fn 항목은_카멜_케이스로_쓰고_없는_값은_생략한다() {
        let entry = TrafficEntry {
            request_id: "7".into(),
            url: "https://a.test/".into(),
            method: "GET".into(),
            resource_type: "Document".into(),
            started_at: 10.0,
            ..TrafficEntry::default()
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            json!({
                "requestId": "7", "url": "https://a.test/", "method": "GET",
                "resourceType": "Document", "startedAt": 10.0
            })
        );
        let full: TrafficEntry = serde_json::from_value(json!({
            "requestId": "7", "url": "u", "method": "POST", "resourceType": "Fetch",
            "status": 201, "mimeType": "application/json", "encodedDataLength": 12,
            "startedAt": 1, "completedAt": 2, "endpointKey": "POST https://a.test/x"
        }))
        .unwrap();
        assert_eq!(full.status, Some(201));
        assert_eq!(full.encoded_data_length, Some(12));
        assert_eq!(full.completed_at, Some(2.0));
        assert_eq!(full.endpoint_key.as_deref(), Some("POST https://a.test/x"));
    }

    #[test]
    fn 엔드포인트는_카멜_케이스_필드를_쓴다() {
        let endpoint = ApiEndpoint {
            key: "GET https://a.test/u/:id".into(),
            origin: "https://a.test".into(),
            host: "a.test".into(),
            path: "/u/:id".into(),
            method: "GET".into(),
            count: 2,
            last_request_id: "9".into(),
            last_seen: 5.0,
            statuses: vec![200, 304],
            auth: false,
        };
        assert_eq!(
            serde_json::to_value(&endpoint).unwrap(),
            json!({
                "key": "GET https://a.test/u/:id", "origin": "https://a.test", "host": "a.test",
                "path": "/u/:id", "method": "GET", "count": 2, "lastRequestId": "9",
                "lastSeen": 5.0, "statuses": [200, 304], "auth": false
            })
        );
    }
}
