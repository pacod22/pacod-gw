-- Audio and image tokens, and the units that image, transcription and rerank models are priced
-- by: images made, milliseconds of audio and search units. The four token counts are parts of
-- `input_tokens` and `output_tokens`, kept so that a call's cost can be explained. Rows written
-- before this migration read as zero, which is what they used.
--
-- The core sends these fields on every event, and an older analytics service drops the fields
-- it does not know: roll out the analytics service before the core (runbook section 6).
ALTER TABLE usage_events
    ADD COLUMN IF NOT EXISTS audio_input_tokens  UInt64 DEFAULT 0 AFTER server_tool_calls,
    ADD COLUMN IF NOT EXISTS image_input_tokens  UInt64 DEFAULT 0 AFTER audio_input_tokens,
    ADD COLUMN IF NOT EXISTS audio_output_tokens UInt64 DEFAULT 0 AFTER image_input_tokens,
    ADD COLUMN IF NOT EXISTS image_output_tokens UInt64 DEFAULT 0 AFTER audio_output_tokens,
    ADD COLUMN IF NOT EXISTS images              UInt64 DEFAULT 0 AFTER image_output_tokens,
    ADD COLUMN IF NOT EXISTS audio_ms            UInt64 DEFAULT 0 AFTER images,
    ADD COLUMN IF NOT EXISTS search_units        UInt64 DEFAULT 0 AFTER audio_ms;
