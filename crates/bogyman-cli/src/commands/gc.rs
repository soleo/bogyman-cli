use anyhow::Result;
use time::OffsetDateTime;

use crate::{Ctx, EXIT_OK, json, refresh};

pub fn run(ctx: &Ctx) -> Result<u8> {
    let mut store = refresh::open_store(ctx)?;
    let days = ctx.config.store.retention_days;
    let (events, sessions) = store.gc(days, OffsetDateTime::now_utc())?;
    if ctx.json {
        json::ok(
            ctx,
            "gc",
            serde_json::json!({"retention_days": days, "events_removed": events, "sessions_removed": sessions}),
        )?;
    } else {
        println!("Removed {events} events and {sessions} sessions older than {days} days.");
    }
    Ok(EXIT_OK)
}
