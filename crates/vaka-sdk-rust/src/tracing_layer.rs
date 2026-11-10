//! `tracing` integration: events become breadcrumbs, and ERROR events are
//! captured as errors.
//!
//! ```ignore
//! use tracing_subscriber::prelude::*;
//! tracing_subscriber::registry().with(vaka_sdk::tracing_layer::VakaLayer::default()).init();
//! ```

use crate::Level;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Sends `tracing` events to Vaka.
pub struct VakaLayer {
    /// Capture ERROR-level events as Vaka errors (default true).
    pub capture_errors: bool,
}

impl Default for VakaLayer {
    fn default() -> Self {
        Self { capture_errors: true }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<String>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl<S: Subscriber> Layer<S> for VakaLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut v = MessageVisitor::default();
        event.record(&mut v);
        let mut msg = v.message;
        if !v.fields.is_empty() {
            msg = format!("{msg} {}", v.fields.join(" "));
        }
        let level = match *meta.level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warning,
            _ => Level::Info,
        };
        if level == Level::Error && self.capture_errors {
            crate::capture_message(&msg, Level::Error);
        }
        crate::add_breadcrumb(meta.target(), &msg, level);
    }
}
