-- pacod-broker (runbook section 20): the time to first token of a streamed answer, and the
-- broker ticket a caller quoted, which gw-broker reports on. Rows written before this migration
-- read as zero and empty: not measured, no ticket.
--
-- The ticket is a proof of the caller's interaction on the broker: reports leave it out
-- (SELECT * EXCEPT), and only /reports/requests with broker_ticket=only returns it, which the
-- core serves to the broker_bridge role alone.
--
-- The core sends these fields on every event, and an older analytics service drops the fields
-- it does not know: roll out the analytics service before the core (runbook section 6).
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS first_token_ms Float32 DEFAULT 0 AFTER upstream_stream_ms,
    ADD COLUMN IF NOT EXISTS broker_ticket  String  DEFAULT '';
