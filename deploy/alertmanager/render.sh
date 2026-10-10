#!/bin/sh
# Writes the Alertmanager configuration from the environment, then starts Alertmanager.
#
# Alertmanager's configuration file takes no environment variables, so it is written here at
# every start, into /tmp:
#   - an email receiver when PGW_SMTP_HOST and PGW_ALERT_EMAIL_TO are set, with the SMTP
#     account of the account emails (PGW_SMTP_*, PGW_EMAIL_FROM_ADDRESS). The password goes
#     into a file of its own (smtp_auth_password_file), never into the configuration;
#   - a webhook receiver when PGW_ALERT_WEBHOOK_URL is set;
#   - neither: alerts stay on Prometheus' Alerts page and in Grafana.
set -eu

config=/tmp/alertmanager.yml
password_file=/tmp/smtp_password

# A value for a single-quoted YAML scalar.
quote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

host=${PGW_SMTP_HOST:-}
to=${PGW_ALERT_EMAIL_TO:-}
webhook=${PGW_ALERT_WEBHOOK_URL:-}

{
    if [ -n "$host" ] && [ -n "$to" ]; then
        tls=${PGW_SMTP_TLS:-starttls}
        case "$tls" in
            tls) port=${PGW_SMTP_PORT:-465} ;;
            none) port=${PGW_SMTP_PORT:-25} ;;
            *) port=${PGW_SMTP_PORT:-587} ;;
        esac
        echo "global:"
        echo "  smtp_smarthost: $(quote "$host:$port")"
        # The sender of the account emails; without one, the SMTP username, as for those.
        from=${PGW_EMAIL_FROM_ADDRESS:-${PGW_SMTP_USERNAME:-alerts@localhost}}
        echo "  smtp_from: $(quote "$from")"
        # STARTTLS is required unless the connection is TLS from the first byte (port 465,
        # which Alertmanager recognises) or explicitly unencrypted.
        if [ "$tls" = "starttls" ]; then echo "  smtp_require_tls: true"; else echo "  smtp_require_tls: false"; fi
        if [ -n "${PGW_SMTP_USERNAME:-}" ]; then
            echo "  smtp_auth_username: $(quote "$PGW_SMTP_USERNAME")"
            umask 077
            printf '%s' "${PGW_SMTP_PASSWORD:-}" > "$password_file"
            echo "  smtp_auth_password_file: $password_file"
        fi
    fi
    echo "route:"
    echo "  receiver: default"
    echo "  group_by: [alertname, provider, model, instance, mode]"
    echo "  group_wait: 30s"
    echo "  group_interval: 5m"
    echo "  repeat_interval: 4h"
    echo "receivers:"
    echo "  - name: default"
    if [ -n "$host" ] && [ -n "$to" ]; then
        echo "    email_configs:"
        echo "      - to: $(quote "$to")"
        echo "        send_resolved: true"
    fi
    if [ -n "$webhook" ]; then
        echo "    webhook_configs:"
        echo "      - url: $(quote "$webhook")"
        echo "        send_resolved: true"
    fi
} > "$config"

if [ "${1:-}" = "--render-only" ]; then
    exit 0
fi
exec /bin/alertmanager --config.file="$config" --storage.path=/alertmanager "$@"
