-- pacod-id, version 1: the tables of docs/concept.md, appendix A.
--
-- Conventions: ids are text with a prefix (usr_, svc_, org_, rb_, inv_, sid_, grt_, …), times
-- are timestamptz, secrets are never stored in the clear (SHA-256 or HMAC as bytea, or sealed
-- under the key-encryption key as JSON). Migrations only add.

-- Accounts ---------------------------------------------------------------------------------

CREATE TABLE accounts (
    id                    text        PRIMARY KEY,
    -- Lowercased; unique.
    email                 text        NOT NULL,
    email_verified_at     timestamptz,
    -- How the address was proven: verified (a link), admin, federated (Google said so),
    -- invitation (accepted from the mailed link), legacy_assumed (imported without proof).
    email_verified_source text        NOT NULL DEFAULT ''
        CHECK (email_verified_source IN ('', 'verified', 'admin', 'federated', 'invitation', 'legacy_assumed')),
    display_name          text        NOT NULL DEFAULT '',
    locale                text        NOT NULL DEFAULT '',
    status                text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    -- A PHC string: $argon2id$… or, until the next sign-in, an imported $2b$… bcrypt hash.
    password_hash         text,
    password_changed_at   timestamptz,
    -- Must the owner choose a new password at the next sign-in (too short for their roles)?
    password_change_due   boolean     NOT NULL DEFAULT false,
    signup_realm          text        NOT NULL DEFAULT '',
    last_realm            text        NOT NULL DEFAULT '',
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    last_login_at         timestamptz
);
CREATE UNIQUE INDEX accounts_email ON accounts (email);

-- Ids an account had in a product before it moved here (pacod-gw usr_…, PACOD's user ids).
CREATE TABLE account_external_ids (
    source      text NOT NULL,
    external_id text NOT NULL,
    account_id  text NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    PRIMARY KEY (source, external_id)
);
CREATE INDEX account_external_ids_account ON account_external_ids (account_id);

-- Sign-in with Google (and later other providers), keyed by the provider's subject.
CREATE TABLE federated_identities (
    provider         text        NOT NULL,
    provider_subject text        NOT NULL,
    account_id       text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email_at_link    text        NOT NULL DEFAULT '',
    created_at       timestamptz NOT NULL DEFAULT now(),
    last_used_at     timestamptz,
    PRIMARY KEY (provider, provider_subject)
);
CREATE INDEX federated_identities_account ON federated_identities (account_id);

-- Terms and privacy documents accepted, per realm.
CREATE TABLE consents (
    account_id  text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    realm       text        NOT NULL,
    document    text        NOT NULL,
    version     text        NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, realm, document)
);

-- Machines ---------------------------------------------------------------------------------

CREATE TABLE service_accounts (
    id              text        PRIMARY KEY,
    name            text        NOT NULL,
    -- NULL: the platform's.
    organisation_id text,
    status          text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_by      text        NOT NULL DEFAULT '',
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE service_tokens (
    id                 text        PRIMARY KEY,
    service_account_id text        NOT NULL REFERENCES service_accounts (id) ON DELETE CASCADE,
    -- HMAC-SHA256 of a pidt_ token with PID_TOKEN_PEPPER.
    token_hmac         bytea,
    -- An imported product token (pgwa_…): its HMAC under that product's pepper.
    legacy_hmac        bytea,
    legacy_pepper_id   text,
    hint               text        NOT NULL DEFAULT '',
    expires_at         timestamptz,
    last_used_at       timestamptz,
    status             text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    created_at         timestamptz NOT NULL DEFAULT now(),
    CHECK (token_hmac IS NOT NULL OR legacy_hmac IS NOT NULL)
);
CREATE UNIQUE INDEX service_tokens_hmac ON service_tokens (token_hmac) WHERE token_hmac IS NOT NULL;
CREATE UNIQUE INDEX service_tokens_legacy ON service_tokens (legacy_pepper_id, legacy_hmac) WHERE legacy_hmac IS NOT NULL;

-- Authorization ----------------------------------------------------------------------------

CREATE TABLE organisations (
    id           text        PRIMARY KEY,
    slug         text        NOT NULL UNIQUE,
    display_name text        NOT NULL DEFAULT '',
    status       text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    -- Built into a product (the gateway's _system organisation): never deleted.
    system       boolean     NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE organisation_products (
    organisation_id text        NOT NULL REFERENCES organisations (id) ON DELETE CASCADE,
    product         text        NOT NULL,
    registered_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organisation_id, product)
);

-- Scopes products register below organisations or the platform (gw.workspace, pacod.profile).
CREATE TABLE scopes (
    type         text        NOT NULL,
    id           text        NOT NULL,
    parent_type  text        NOT NULL,
    -- '' for the platform.
    parent_id    text        NOT NULL DEFAULT '',
    product      text        NOT NULL,
    display_name text        NOT NULL DEFAULT '',
    -- Built into the product (the gateway's _system workspace): never deleted.
    system       boolean     NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (type, id)
);
CREATE INDEX scopes_parent ON scopes (parent_type, parent_id);

-- Each product's manifest, as its last push sent it.
CREATE TABLE catalog (
    product   text        PRIMARY KEY,
    version   bigint      NOT NULL DEFAULT 1,
    manifest  jsonb       NOT NULL,
    pushed_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE custom_roles (
    product     text        NOT NULL,
    name        text        NOT NULL,
    description text        NOT NULL DEFAULT '',
    created_by  text        NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (product, name)
);

CREATE TABLE role_bindings (
    id             text        PRIMARY KEY,
    principal_id   text        NOT NULL,
    principal_kind text        NOT NULL CHECK (principal_kind IN ('user', 'service')),
    -- Qualified: gw.org_admin, pacod.custom.tester, id.admin.
    role           text        NOT NULL,
    scope_type     text        NOT NULL,
    -- '' for the platform.
    scope_id       text        NOT NULL DEFAULT '',
    source         text        NOT NULL DEFAULT 'manual'
        CHECK (source IN ('manual', 'invitation', 'bootstrap', 'import', 'policy')),
    created_by     text        NOT NULL DEFAULT '',
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (principal_id, role, scope_type, scope_id)
);
CREATE INDEX role_bindings_scope ON role_bindings (scope_type, scope_id);

CREATE TABLE invitations (
    id         text        PRIMARY KEY,
    token_hash bytea       NOT NULL UNIQUE,
    email      text        NOT NULL,
    role       text        NOT NULL,
    scope_type text        NOT NULL,
    scope_id   text        NOT NULL DEFAULT '',
    realm      text        NOT NULL,
    invited_by text        NOT NULL DEFAULT '',
    -- Sent by mail: accepting it proves the address.
    emailed    boolean     NOT NULL DEFAULT false,
    -- How the inviting product names the role and the scope, for the mail and the page.
    labels     jsonb       NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    UNIQUE (email, scope_type, scope_id)
);

-- Every authorization write takes this row and raises the version, so the change feed is
-- ordered: a follower never skips a change that committed late (the gateway's config_head).
CREATE TABLE authz_head (
    id      boolean PRIMARY KEY DEFAULT true CHECK (id),
    version bigint  NOT NULL
);
INSERT INTO authz_head (id, version) VALUES (true, 0);

CREATE TABLE authz_changes (
    version      bigint      PRIMARY KEY,
    at           timestamptz NOT NULL DEFAULT now(),
    kind         text        NOT NULL,
    product      text,
    principal_id text,
    payload      jsonb       NOT NULL DEFAULT '{}'
);
CREATE INDEX authz_changes_at ON authz_changes (at);

-- OpenID Connect ---------------------------------------------------------------------------

-- Clients come from PID_CONFIG; this table holds what the file does not: the secret's hash.
CREATE TABLE clients (
    id          text        PRIMARY KEY,
    realm       text        NOT NULL,
    -- HMAC-SHA256 of the client secret with PID_TOKEN_PEPPER.
    secret_hmac bytea,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- A sign-in in progress, from /oidc/authorize to the code.
CREATE TABLE auth_flows (
    id           text        PRIMARY KEY,
    realm        text        NOT NULL,
    client_id    text        NOT NULL,
    -- The authorization request: redirect_uri, state, nonce, PKCE challenge, scope, prompt, …
    request      jsonb       NOT NULL,
    -- SHA-256 of the flow cookie, which binds the flow to one browser.
    browser_hash bytea       NOT NULL,
    account_id   text        REFERENCES accounts (id) ON DELETE CASCADE,
    stage        text        NOT NULL DEFAULT 'start',
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL
);

CREATE TABLE auth_codes (
    code_hash      bytea       PRIMARY KEY,
    account_id     text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    sso_session_id text,
    client_id      text        NOT NULL,
    redirect_uri   text        NOT NULL,
    pkce_challenge text        NOT NULL,
    nonce          text,
    scope          text        NOT NULL,
    auth_time      timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL,
    used_at        timestamptz
);

-- A browser's single sign-on session at one realm; its id is the sid claim.
CREATE TABLE sso_sessions (
    id             text        PRIMARY KEY,
    account_id     text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    realm          text        NOT NULL,
    cookie_hash    bytea       NOT NULL UNIQUE,
    remember_me    boolean     NOT NULL DEFAULT false,
    auth_time      timestamptz NOT NULL,
    amr            text[]      NOT NULL DEFAULT '{}',
    acr            text        NOT NULL DEFAULT '',
    ip             text        NOT NULL DEFAULT '',
    user_agent     text        NOT NULL DEFAULT '',
    device_label   text        NOT NULL DEFAULT '',
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL,
    revoked_at     timestamptz,
    revoked_reason text
);
CREATE INDEX sso_sessions_account ON sso_sessions (account_id);

-- What a client holds for an account: a refresh token, rotated on every use.
CREATE TABLE grants (
    id              text        PRIMARY KEY,
    -- NULL for native apps and delegated grants without a browser session.
    sso_session_id  text        REFERENCES sso_sessions (id) ON DELETE CASCADE,
    -- The sid tokens of this grant carry: the session's, or the delegating session's.
    sid             text        NOT NULL,
    client_id       text        NOT NULL,
    account_id      text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    scope           text        NOT NULL,
    -- SHA-256 of the current refresh token, and of the one before it (for the grace window).
    refresh_hash    bytea       NOT NULL UNIQUE,
    previous_hash   bytea,
    rotated_at      timestamptz,
    -- The successor of previous_hash, sealed under the key-encryption key, for 30 seconds.
    successor_enc   jsonb,
    successor_until timestamptz,
    -- A delegated grant (token exchange): the client acting for the account.
    act_sub         text,
    auth_time       timestamptz NOT NULL,
    amr             text[]      NOT NULL DEFAULT '{}',
    idle_expires_at timestamptz,
    expires_at      timestamptz NOT NULL,
    revoked_at      timestamptz,
    -- Imported from a product's own sessions; may be claimed by any client of its list.
    legacy_clients  text[],
    -- The code this grant came from: a code presented twice revokes its grant (RFC 6749, 4.1.2).
    code_hash       bytea,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_used_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX grants_account ON grants (account_id);
CREATE INDEX grants_previous ON grants (previous_hash) WHERE previous_hash IS NOT NULL;
CREATE INDEX grants_sid ON grants (sid);
CREATE INDEX grants_code ON grants (code_hash) WHERE code_hash IS NOT NULL;

-- Earlier refresh tokens of a grant, rotated away before its previous one: presenting one again
-- is reuse (concept 5.4, decision 9), while a secret the grant never had is only invalid.
CREATE TABLE refresh_spent (
    hash     bytea PRIMARY KEY,
    grant_id text  NOT NULL REFERENCES grants (id) ON DELETE CASCADE
);
CREATE INDEX refresh_spent_grant ON refresh_spent (grant_id);

CREATE TABLE signing_keys (
    kid          text        PRIMARY KEY,
    alg          text        NOT NULL CHECK (alg IN ('RS256', 'ES256')),
    -- next: published, not yet signing; active: signs; retired: published until retires_at.
    state        text        NOT NULL CHECK (state IN ('next', 'active', 'retired')),
    public_jwk   jsonb       NOT NULL,
    -- The PKCS#8 private key, sealed under the key-encryption key (the kid is the context).
    private_enc  jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    activates_at timestamptz NOT NULL,
    retires_at   timestamptz
);
-- One signing key per algorithm.
CREATE UNIQUE INDEX signing_keys_active ON signing_keys (alg) WHERE state = 'active';

CREATE TABLE backchannel_outbox (
    id              bigserial   PRIMARY KEY,
    client_id       text        NOT NULL,
    -- The logout token's claims without iat, exp and jti: each attempt is signed afresh, so a
    -- retry after the token's two minutes still carries a valid one.
    claims          jsonb       NOT NULL,
    attempts        integer     NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    done_at         timestamptz,
    last_error      text        NOT NULL DEFAULT '',
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX backchannel_outbox_due ON backchannel_outbox (next_attempt_at) WHERE done_at IS NULL;

-- Support ----------------------------------------------------------------------------------

-- Email verification and password reset links: single use, bound to the address.
CREATE TABLE account_tokens (
    token_hash bytea       PRIMARY KEY,
    -- change_email: sent to the new address; `email` is the address it confirms.
    purpose    text        NOT NULL CHECK (purpose IN ('verify_email', 'reset_password', 'change_email')),
    account_id text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    email      text        NOT NULL,
    realm      text        NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX account_tokens_account ON account_tokens (account_id, purpose);

-- Counters for sign-in and mail limits, shared by every instance.
CREATE TABLE throttle (
    bucket       text        NOT NULL,
    key          text        NOT NULL,
    window_start timestamptz NOT NULL,
    count        integer     NOT NULL DEFAULT 0,
    locked_until timestamptz,
    PRIMARY KEY (bucket, key)
);

-- Browsers on which an account has signed in before: they keep signing in during a lockout.
CREATE TABLE known_devices (
    device_hash   bytea       NOT NULL,
    account_id    text        NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (device_hash, account_id)
);

CREATE TABLE mail_outbox (
    id              bigserial   PRIMARY KEY,
    template        text        NOT NULL,
    realm           text        NOT NULL,
    to_address      text        NOT NULL,
    params          jsonb       NOT NULL DEFAULT '{}',
    attempts        integer     NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NOT NULL DEFAULT now(),
    sent_at         timestamptz,
    error           text        NOT NULL DEFAULT '',
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX mail_outbox_due ON mail_outbox (next_attempt_at) WHERE sent_at IS NULL;

CREATE TABLE audit_log (
    id              bigserial   PRIMARY KEY,
    at              timestamptz NOT NULL DEFAULT now(),
    actor           text        NOT NULL DEFAULT '',
    actor_kind      text        NOT NULL DEFAULT '',
    action          text        NOT NULL,
    subject         text        NOT NULL DEFAULT '',
    organisation_id text,
    scope_type      text,
    scope_id        text,
    client_id       text,
    realm           text,
    ip              text        NOT NULL DEFAULT '',
    user_agent      text        NOT NULL DEFAULT '',
    detail          jsonb       NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_log_at ON audit_log (at);
CREATE INDEX audit_log_subject ON audit_log (subject);
CREATE INDEX audit_log_actor ON audit_log (actor);
CREATE INDEX audit_log_organisation ON audit_log (organisation_id) WHERE organisation_id IS NOT NULL;

-- A deleted account, until every product has deleted its own data (concept 6.9).
CREATE TABLE account_deletions (
    account_id       text        PRIMARY KEY,
    requested_at     timestamptz NOT NULL DEFAULT now(),
    requested_by     text        NOT NULL DEFAULT '',
    pending_products text[]      NOT NULL DEFAULT '{}',
    -- The account's ids in products (source → id), which the products may need to find it.
    external_ids     jsonb       NOT NULL DEFAULT '{}'
);

-- Background jobs: one instance at a time holds a job's lease.
CREATE TABLE jobs (
    name        text        PRIMARY KEY,
    holder      text        NOT NULL DEFAULT '',
    lease_until timestamptz,
    last_run_at timestamptz
);

-- Running instances, with a check value of their key-encryption key: rewrap waits until
-- every running instance has the current one.
CREATE TABLE instances (
    id         text        PRIMARY KEY,
    kek_id     text        NOT NULL,
    kek_check  text        NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(),
    seen_at    timestamptz NOT NULL DEFAULT now()
);

-- Imports ----------------------------------------------------------------------------------

-- Imports from a product's own sign-in (concept 17): where the feed and the audit log stood, so
-- that a rollback to the product's sign-in, or a second import, is refused once something was
-- written at pacod-id since.
CREATE TABLE imports (
    id             bigserial   PRIMARY KEY,
    product        text        NOT NULL,
    at             timestamptz NOT NULL DEFAULT now(),
    -- authz_head and max(audit_log.id) right after the import.
    feed_seq       bigint      NOT NULL,
    audit_id       bigint      NOT NULL,
    -- The product's configuration version and time the export was read at.
    source_version bigint      NOT NULL,
    exported_at    timestamptz NOT NULL,
    report         jsonb       NOT NULL DEFAULT '{}'
);
CREATE INDEX imports_product ON imports (product, id);
