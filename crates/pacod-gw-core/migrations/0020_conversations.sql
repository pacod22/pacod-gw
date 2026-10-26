-- Conversations of OpenAI's Responses API (/v1/conversations) that the gateway keeps for a
-- workspace whose setting responses.store is on: their items in order, which a call that names
-- the conversation is given before its own input, and to which its input and answer are added.
-- A conversation expires its retention after it was last added to; its items go with it.
-- Additive: an older instance ignores the tables.
CREATE TABLE IF NOT EXISTS conversations (
    -- conv_<uuid>, minted by the gateway.
    id               text PRIMARY KEY,
    organisation_id  text NOT NULL,
    workspace_id     text NOT NULL,
    key_id           text NOT NULL,
    -- Who began it.
    end_user         text NOT NULL DEFAULT '',
    user_id          text NOT NULL DEFAULT '',
    metadata         jsonb NOT NULL DEFAULT '{}',
    created_at       timestamptz NOT NULL DEFAULT now(),
    used_at          timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS conversations_by_end_user ON conversations (workspace_id, end_user);
CREATE INDEX IF NOT EXISTS conversations_by_user ON conversations (workspace_id, user_id);
CREATE INDEX IF NOT EXISTS conversations_by_expiry ON conversations (expires_at);

CREATE TABLE IF NOT EXISTS conversation_items (
    conversation_id  text NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    -- The order of the items.
    seq              bigserial,
    -- The item's own id, or one the gateway gave it; never written into the item, which a
    -- provider is given as it was sent or answered.
    item_id          text NOT NULL,
    workspace_id     text NOT NULL,
    -- Whose call brought the item.
    end_user         text NOT NULL DEFAULT '',
    user_id          text NOT NULL DEFAULT '',
    -- The response whose input or answer the item was, if any.
    response_id      text,
    -- The item as JSON, kept as bytes like a stored response's.
    item             bytea NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (conversation_id, seq)
);
CREATE INDEX IF NOT EXISTS conversation_items_by_id ON conversation_items (conversation_id, item_id);
CREATE INDEX IF NOT EXISTS conversation_items_by_end_user ON conversation_items (workspace_id, end_user);
CREATE INDEX IF NOT EXISTS conversation_items_by_user ON conversation_items (workspace_id, user_id);
