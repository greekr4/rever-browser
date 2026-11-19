//! Port of `src/main/providers/turn-pulse.ts` — tells the chat UI a turn is still in
//! progress. Grok can think for minutes before the first token, and Claude Code can sit
//! inside one tool longer than 60s. Those gaps are not a dead agent.
//!
//! The TypeScript owns a `setInterval`; here the host owns the timer and asks
//! `TurnPulse::due` how many pulses to send at a given time.

use serde_json::{Value, json};

/// Shorter than the UI's 60s silence watchdog, so a quiet-but-live turn is not cancelled.
pub const TURN_PULSE_MS: u64 = 15_000;

/// The notification one pulse sends.
pub fn pulse_notification(session_id: &str) -> Value {
    json!({ "sessionId": session_id, "update": { "sessionUpdate": "rever_pulse" } })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnPulse {
    session_id: String,
    interval_ms: u64,
    /// None once stopped.
    next_at_ms: Option<u64>,
}

impl TurnPulse {
    /// Start when the turn starts; `TURN_PULSE_MS` is the usual interval.
    /// Call `stop` when the turn ends — including when it fails.
    pub fn start(session_id: impl Into<String>, now_ms: u64, interval_ms: u64) -> Self {
        // Node runs a 0ms interval as 1ms; it also keeps `due` from dividing by zero.
        let interval_ms = interval_ms.max(1);
        Self {
            session_id: session_id.into(),
            interval_ms,
            next_at_ms: Some(now_ms.saturating_add(interval_ms)),
        }
    }

    /// How many pulses have come due by `now_ms`; each one is reported once.
    pub fn due(&mut self, now_ms: u64) -> u64 {
        let Some(next_at_ms) = self.next_at_ms.filter(|next| now_ms >= *next) else {
            return 0;
        };
        let due = (now_ms - next_at_ms) / self.interval_ms + 1;
        self.next_at_ms = Some(next_at_ms.saturating_add(due.saturating_mul(self.interval_ms)));
        due
    }

    pub fn notification(&self) -> Value {
        pulse_notification(&self.session_id)
    }

    pub fn stop(&mut self) {
        self.next_at_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 간격마다_rever_pulse를_보내고_멈추면_더_보내지_않는다() {
        let mut seen: Vec<String> = Vec::new();
        let mut pulse = TurnPulse::start("grok:1", 0, 15_000);
        let mut advance_to = |pulse: &mut TurnPulse, now_ms: u64| {
            for _ in 0..pulse.due(now_ms) {
                let notification = pulse.notification();
                seen.push(
                    notification["update"]["sessionUpdate"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            seen.clone()
        };

        assert_eq!(advance_to(&mut pulse, 15_000), ["rever_pulse"]);
        assert_eq!(advance_to(&mut pulse, 45_000).len(), 3);

        pulse.stop();
        assert_eq!(advance_to(&mut pulse, 105_000).len(), 3);
    }
}
