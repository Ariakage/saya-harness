//! CLI rendering of shared application events.
use super::*;
pub use saya_app::events::{EventModel, EventStore, EventWriter, timestamp};
use saya_core::contract::Event;
pub struct StdoutSink;
impl saya_app::events::EventSink for StdoutSink {
    fn publish<'a>(&'a self, event: &'a Event) -> Operation<'a, ()> {
        Box::pin(async move {
            let mut stdout = io::stdout().lock();
            serde_json::to_writer(&mut stdout, event)
                .map_err(|_| cli_error("output_error", "cannot write event"))?;
            stdout
                .write_all(b"\n")
                .and_then(|_| stdout.flush())
                .map_err(|_| cli_error("output_error", "cannot write event"))
        })
    }
}
