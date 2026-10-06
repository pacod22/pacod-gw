-- One-time links sent by email: email verification and password reset.
--
-- Only the SHA-256 of a token is stored; the token itself exists only in the email. A token is
-- bound to the address it was sent to, so it stops working when the user's email changes.
-- Issuing a new token for a user and purpose replaces the previous one.

CREATE TABLE account_tokens (
    token_hash bytea PRIMARY KEY,
    purpose text NOT NULL CHECK (purpose IN ('verify_email', 'reset_password')),
    user_id text NOT NULL,
    email text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX account_tokens_user ON account_tokens (user_id, purpose);
CREATE INDEX account_tokens_expires ON account_tokens (expires_at);
