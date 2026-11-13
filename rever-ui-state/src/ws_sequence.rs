//! Port of `src/renderer/src/lib/ws-sequence.ts` — sending a list of WebSocket
//! frames in order, with a pause before each.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsStep {
    pub payload: String,
    /// Wait this long before sending the frame. A JavaScript number: the
    /// input field allows fractions, so this is not an integer.
    pub delay_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_binary: Option<bool>,
}

/// What the `send` callback reports for one frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsSendResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsSequenceResult {
    /// How many frames went out before the sequence ended.
    pub sent: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Sends the steps in order and stops at the first one that fails.
/// Only the first step may reopen a closed socket: a server that drops the
/// connection mid-sequence has rejected it, and reconnecting would hide that.
///
/// `send` gets the step and whether it may reconnect. `sleep` waits the given
/// milliseconds; there is no default, since a timer belongs to the caller's
/// executor.
pub async fn run_sequence(
    steps: &[WsStep],
    mut send: impl AsyncFnMut(&WsStep, bool) -> WsSendResult,
    mut sleep: impl AsyncFnMut(f64),
) -> WsSequenceResult {
    for (i, step) in steps.iter().enumerate() {
        if step.delay_ms > 0.0 {
            sleep(step.delay_ms).await;
        }
        let r = send(step, i == 0).await;
        if !r.ok {
            return WsSequenceResult {
                sent: i,
                error: Some(r.error.unwrap_or_else(|| "error".to_owned())),
            };
        }
    }
    WsSequenceResult {
        sent: steps.len(),
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::*;

    // The callbacks in these tests never wait, so polling until ready is a
    // complete executor for them.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
        }
    }

    fn step(payload: &str, delay_ms: f64) -> WsStep {
        WsStep {
            payload: payload.to_owned(),
            delay_ms,
            as_binary: None,
        }
    }

    fn ok() -> WsSendResult {
        WsSendResult {
            ok: true,
            error: None,
        }
    }

    // --- ws-sequence.test.ts ---

    #[test]
    fn 순서대로_보내고_각_프레임_앞에서_지정한_시간만큼_기다린다() {
        let log = RefCell::new(Vec::new());
        let r = block_on(run_sequence(
            &[step("auth", 0.0), step("subscribe", 100.0)],
            async |s, _| {
                log.borrow_mut().push(format!("send:{}", s.payload));
                ok()
            },
            async |ms| log.borrow_mut().push(format!("wait:{ms}")),
        ));
        assert_eq!(
            r,
            WsSequenceResult {
                sent: 2,
                error: None
            }
        );
        assert_eq!(*log.borrow(), ["send:auth", "wait:100", "send:subscribe"]);
    }

    #[test]
    fn 첫_프레임만_재연결을_허용한다() {
        let mut flags = Vec::new();
        block_on(run_sequence(
            &[step("a", 0.0), step("b", 0.0), step("c", 0.0)],
            async |_s, reconnect| {
                flags.push(reconnect);
                ok()
            },
            async |_| {},
        ));
        assert_eq!(flags, [true, false, false]);
    }

    #[test]
    fn 실패한_단계에서_멈추고_몇_개를_보냈는지와_사유를_돌려준다() {
        let mut sent = Vec::new();
        let r = block_on(run_sequence(
            &[step("a", 0.0), step("b", 0.0), step("c", 0.0)],
            async |s, _| {
                if s.payload == "b" {
                    return WsSendResult {
                        ok: false,
                        error: Some("the socket is no longer open".to_owned()),
                    };
                }
                sent.push(s.payload.clone());
                ok()
            },
            async |_| {},
        ));
        assert_eq!(
            r,
            WsSequenceResult {
                sent: 1,
                error: Some("the socket is no longer open".to_owned())
            }
        );
        assert_eq!(sent, ["a"]);
    }

    // --- not covered by ws-sequence.test.ts ---

    #[test]
    fn 사유_없는_실패는_error로_보고한다() {
        let r = block_on(run_sequence(
            &[step("a", 0.0)],
            async |_, _| WsSendResult {
                ok: false,
                error: None,
            },
            async |_| {},
        ));
        assert_eq!(
            r,
            WsSequenceResult {
                sent: 0,
                error: Some("error".to_owned())
            }
        );
    }

    #[test]
    fn 실패한_단계의_대기는_이미_끝난_뒤이고_이후_단계는_기다리지도_않는다() {
        let waits = RefCell::new(Vec::new());
        let r = block_on(run_sequence(
            &[step("a", 5.0), step("b", 1.5), step("c", 30.0)],
            async |s, _| WsSendResult {
                ok: s.payload != "b",
                error: None,
            },
            async |ms| waits.borrow_mut().push(ms),
        ));
        assert_eq!(r.sent, 1);
        assert_eq!(*waits.borrow(), [5.0, 1.5]);
    }

    #[test]
    fn 단계가_없으면_아무것도_보내지_않고_0을_돌려준다() {
        let mut calls = 0;
        let r = block_on(run_sequence(
            &[],
            async |_, _| {
                calls += 1;
                ok()
            },
            async |_| {},
        ));
        assert_eq!(
            r,
            WsSequenceResult {
                sent: 0,
                error: None
            }
        );
        assert_eq!(calls, 0);
    }

    #[test]
    fn 음수나_nan_대기_시간은_기다리지_않는다() {
        let waits = RefCell::new(Vec::new());
        block_on(run_sequence(
            &[step("a", -1.0), step("b", f64::NAN)],
            async |_, _| ok(),
            async |ms| waits.borrow_mut().push(ms),
        ));
        assert!(waits.borrow().is_empty());
    }

    #[test]
    fn 단계와_결과는_타입스크립트와_같은_필드_이름으로_직렬화된다() {
        assert_eq!(
            json!(step("ping", 250.0)),
            json!({ "payload": "ping", "delayMs": 250.0 })
        );
        let binary: WsStep =
            serde_json::from_value(json!({ "payload": "AQI=", "delayMs": 0, "asBinary": true }))
                .unwrap();
        assert_eq!(binary.as_binary, Some(true));
        assert_eq!(
            json!(WsSequenceResult {
                sent: 2,
                error: None
            }),
            json!({ "sent": 2 })
        );
        assert_eq!(
            json!(WsSequenceResult {
                sent: 1,
                error: Some("closed".to_owned())
            }),
            json!({ "sent": 1, "error": "closed" })
        );
    }
}
