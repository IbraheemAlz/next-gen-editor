//! Issue #469 — one place that attributes an `Event::Error` reply to the
//! command it answers.
//!
//! Every refusal path (finite guard, story gate, protection gate, table
//! validation, per-handler errors) builds its `Event::Error` with whatever
//! kind and message it has; `Engine::apply` then stamps the dispatched
//! command's wire name (`CommandKind::wire_name`, derived from the same
//! `command_meta!` table as everything else - no hand list) on the reply
//! through [`stamp`]. Construction sites therefore cannot forget it.

use bridge::{CommandKind, Event};

/// Attach `kind`'s wire name to `evt` when it is an unattributed `Event::Error`.
pub(crate) fn stamp(evt: Event, kind: CommandKind) -> Event {
    evt.with_command(|| kind.wire_name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge::Command;

    #[test]
    fn stamps_only_unattributed_errors() {
        let kind = Command::Ping.kind();
        let e = stamp(Event::error("Ping: no"), kind);
        assert!(matches!(&e, Event::Error { command: Some(c), .. } if c == "PING"));
        let keep = Event::Error {
            message: "x".into(),
            kind: None,
            command: Some("OTHER".into()),
        };
        assert!(
            matches!(stamp(keep, kind), Event::Error { command: Some(c), .. } if c == "OTHER")
        );
        assert!(matches!(stamp(Event::Pong, kind), Event::Pong));
    }
}
