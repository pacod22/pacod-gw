-- Invitations to an organisation or one of its workspaces, sent by email or handed on as a
-- link.
--
-- Only the SHA-256 of a token is stored; the token itself is in the email or in the link the
-- inviter copied. Accepting an invitation gives the role and removes the row; so does
-- declining or revoking it. Inviting the same address to the same scope again replaces the
-- pending invitation. `emailed` records whether the link went to the address itself, which
-- confirms the address when the invitation is accepted.

CREATE TABLE invitations (
    id text PRIMARY KEY,
    token_hash bytea NOT NULL UNIQUE,
    email text NOT NULL,
    role text NOT NULL,
    organisation_id text NOT NULL,
    workspace_id text,
    invited_by text NOT NULL,
    emailed boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE UNIQUE INDEX invitations_one_per_scope ON invitations (email, organisation_id, (coalesce(workspace_id, '')));
CREATE INDEX invitations_email ON invitations (email);
CREATE INDEX invitations_expires ON invitations (expires_at);
