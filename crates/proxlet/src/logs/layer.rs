//! Event records from the server's own `tracing` messages (design decision 3).

use std::fmt::{self, Write as _};
use std::time::SystemTime;

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

use super::writer::{Entry, LogSender};

/// Sends info-and-above messages from proxlet's crates to the log writer.
#[derive(Debug, Clone)]
pub struct RecordLayer {
    sender: LogSender,
}

impl RecordLayer {
    /// A layer sending to `sender`.
    #[must_use]
    pub fn new(sender: LogSender) -> Self {
        Self { sender }
    }

    /// Whether an event is recorded.
    #[must_use]
    pub fn wants(meta: &Metadata<'_>) -> bool {
        meta.target().starts_with("proxlet") && *meta.level() <= Level::INFO
    }
}

impl<S: Subscriber> Layer<S> for RecordLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !Self::wants(meta) {
            return;
        }
        let level = match *meta.level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => "info",
        };
        let mut text = Text::default();
        event.record(&mut text);
        self.sender.send(Entry::Event {
            time: SystemTime::now(),
            level,
            msg: text.finish(),
        });
    }
}

/// The message, then any other fields as `key=value`.
#[derive(Default)]
struct Text {
    message: String,
    fields: String,
}

impl Text {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields.trim_start().to_owned()
        } else {
            self.message + &self.fields
        }
    }
}

impl Visit for Text {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    use super::*;
    use crate::logs::files::Limits;
    use crate::logs::record::Line;
    use crate::logs::writer;

    #[test]
    fn records_proxlet_messages_at_info_and_above() {
        let d = tempfile::tempdir().unwrap();
        let (tx, q) = writer::queue();
        let w = writer::start(
            d.path().to_owned(),
            q,
            std::sync::Arc::new(|| Limits {
                max_size: 1 << 30,
                max_age: Duration::from_secs(60),
            }),
        )
        .unwrap();
        let subscriber = tracing_subscriber::registry().with(RecordLayer::new(tx));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "proxlet::ports", "port {} is free", 80);
            tracing::debug!(target: "proxlet::ports", "not recorded");
            tracing::error!(target: "hyper", "not ours");
            tracing::warn!(target: "proxlet_server", route = "/a", dir = %Path::new("/x").display(), "files directory unavailable");
        });
        w.close();
        let text = std::fs::read_to_string(d.path().join("proxlet.log")).unwrap();
        let got: Vec<(String, String)> = text
            .lines()
            .map(|l| serde_json::from_str::<Line>(l).unwrap())
            .map(|l| (l.level.unwrap(), l.msg.unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                ("info".into(), "port 80 is free".into()),
                (
                    "warn".into(),
                    "files directory unavailable route=/a dir=/x".into()
                ),
            ]
        );
    }
}
