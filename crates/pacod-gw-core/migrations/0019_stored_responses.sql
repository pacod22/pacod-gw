-- Stored responses of OpenAI's Responses API (concept 9.1, ING-05): what the gateway answered
-- in a workspace that keeps responses, so that a later call can continue it with
-- previous_response_id and read it back. A row is one turn: its own input items and the
-- response, linked to the turn before. The provider keeps nothing; the gateway replays the
-- turns. Additive: an older instance ignores the table.
CREATE TABLE IF NOT EXISTS stored_responses (
    -- resp_<the call's request id>, minted by the gateway.
    id               text PRIMARY KEY,
    organisation_id  text NOT NULL,
    workspace_id     text NOT NULL,
    key_id           text NOT NULL,
    end_user         text NOT NULL DEFAULT '',
    user_id          text NOT NULL DEFAULT '',
    -- The turn this one continues.
    previous_id      text,
    -- The conversation (Conversations API) the turn belongs to, if any.
    conversation_id  text,
    -- The model as the caller named it.
    model            text NOT NULL,
    status           text NOT NULL,
    -- This turn's input items (a JSON array) and the response as the caller received it, kept
    -- as bytes: jsonb refuses \u0000 in strings and does not keep the bytes as sent.
    input            bytea NOT NULL,
    response         bytea NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS stored_responses_by_end_user ON stored_responses (workspace_id, end_user);
CREATE INDEX IF NOT EXISTS stored_responses_by_user ON stored_responses (workspace_id, user_id);
CREATE INDEX IF NOT EXISTS stored_responses_by_expiry ON stored_responses (expires_at);
