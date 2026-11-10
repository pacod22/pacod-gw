#!/bin/sh
# The addresses and the main settings of the gateway's running stack: what "make up",
# "make dev-up" and their scale and hub variants print at the end, and "make urls" at any time.
#
# It reads the running containers of the compose project pacod-gw, not deploy/.env. They hold
# what compose made of the files, the overlays (LOAD_TEST, BENCH, DEV_HTTPS) and the defaults,
# so the addresses are those that are really served. Their environments hold secrets, so only
# the variables named below are read, and none of them is a secret. The administrator's
# password is named, not printed; only the development stack's fixed, public one is shown.
#
# It runs on the host in POSIX sh (dash on Debian and Ubuntu, bash 3.2 on macOS). It exits 1
# only when the stack is not running: a value it cannot find is printed as missing, so that a
# successful start never ends in a failure here.

set -u

PROJECT=pacod-gw

# mine NAME: the container runs and belongs to this stack. pacod-caddy, pacod-id and the other
# shared components have the same names in pacod-demo's and pacod-benchmark's stacks.
mine() {
    [ "$(docker inspect -f '{{.State.Running}} {{index .Config.Labels "com.docker.compose.project"}}' "$1" 2>/dev/null)" = "true $PROJECT" ]
}

# env_of CONTAINER NAME: one variable of a container's environment, empty when unset. Only that
# variable leaves this function.
env_of() {
    docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$1" 2>/dev/null | sed -n "s/^$2=//p" | head -n 1
}

# published CONTAINER PORT: the host address of a container's TCP port, IPv4 first; empty when
# the port is not published.
published() {
    out=$(docker port "$1" "$2/tcp" 2>/dev/null) || return 0
    printf '%s\n' "$out" | grep -v '^\[' | head -n 1 | grep . || printf '%s\n' "$out" | head -n 1
}

# This host's address on its network, for ports published on every interface. Asking for a
# route sends nothing.
host_address() {
    a=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
    if [ -z "$a" ] && command -v ipconfig >/dev/null 2>&1; then
        i=$(route -n get default 2>/dev/null | sed -n 's/.*interface: //p')
        [ -z "$i" ] || a=$(ipconfig getifaddr "$i" 2>/dev/null)
    fi
    printf '%s' "${a:-<this host>}"
}

# reach ADDRESS: an address published on every interface becomes this host's own.
reach() {
    case "$1" in
        0.0.0.0:* | "[::]:"*) printf '%s:%s' "$HOST" "${1##*:}" ;;
        *) printf '%s' "$1" ;;
    esac
}

row() { printf '  %-27s %s\n' "$1" "$2"; }

# sites ADDRESSES: the URLs of one of Caddy's sites (GW_API_SITE, GW_CONSOLE_SITE). A port of
# its own (http://:13000) counts where it is published; a host name is served on 443.
sites() {
    for a in $1; do
        case "$a" in
            http://:*)
                p=$(published pacod-caddy "${a#http://:}")
                [ -z "$p" ] || printf 'http://%s\n' "$(reach "$p")" ;;
            http://*) printf '%s\n' "$a" ;;
            *)
                [ -n "$HTTPS" ] || continue
                printf 'https://%s%s\n' "${a#https://}" "$HTTPS_SUFFIX" ;;
        esac
    done
}

if ! mine pacod-caddy; then
    echo "urls: the gateway's stack is not running (no container pacod-caddy of the project $PROJECT)" >&2
    if docker inspect pacod-caddy >/dev/null 2>&1; then
        echo "urls: pacod-caddy belongs to $(docker inspect -f '{{index .Config.Labels "com.docker.compose.project"}}' pacod-caddy)" >&2
    fi
    exit 1
fi

case "$(docker inspect -f '{{index .Config.Labels "com.docker.compose.project.config_files"}}' pacod-caddy)" in
    *standalone.dev.yml*) DEV=1 ;;
    *) DEV= ;;
esac
HOST=$(host_address)
GRAFANA_HOST=
HTTPS=$(published pacod-caddy 443)
HTTPS_SUFFIX=
case "$HTTPS" in *:443 | "") ;; *) HTTPS_SUFFIX=":${HTTPS##*:}" ;; esac

API_SITE=$(env_of pacod-caddy GW_API_SITE)
CONSOLE_SITE=$(env_of pacod-caddy GW_CONSOLE_SITE)
ALLOW=$(env_of pacod-caddy PGW_ADMIN_ALLOW_CIDRS)
AUTH=$(env_of pacod-gw PGW_AUTH)

IMAGE=$(docker inspect -f '{{.Config.Image}}' pacod-gw 2>/dev/null)
echo ""
echo "pacod-gw${DEV:+, development stack}${IMAGE:+ ($IMAGE)}"
echo ""

# Line by line: an address may hold a space ("<this host>").
sites "$CONSOLE_SITE" | while IFS= read -r u; do row "Console" "$u"; done
sites "$API_SITE" | while IFS= read -r u; do
    # In production http://:8088 is published only with BENCH=1, and is listed below then.
    case "$u" in http://*) [ -n "$DEV" ] || continue ;; esac
    row "Data plane" "$u/v1 (OpenAI, Anthropic), /v1beta (Gemini), /gw/v1, /proxy/<provider>, /mcp, /a2a/<name>"
done
if [ -n "$DEV" ]; then
    p=$(published pacod-caddy 13070)
    [ -z "$p" ] || row "Identity (pacod-id)" "http://$(reach "$p")"
else
    h=$(env_of pacod-caddy PGW_ID_HOST)
    [ -z "$h" ] || row "Identity (pacod-id)" "https://$h$HTTPS_SUFFIX"
fi
if mine pacod-grafana; then
    g=$(env_of pacod-grafana GF_SERVER_ROOT_URL)
    case "$g" in */grafana/*) GRAFANA_HOST=$(printf '%s' "$g" | sed 's|^[a-z]*://||; s|[:/].*||') ;; esac
    case "$g" in */grafana/*) g="$g (from PGW_ADMIN_ALLOW_CIDRS only)" ;; esac
    row "Grafana" "$g, user $(env_of pacod-grafana GF_SECURITY_ADMIN_USER | grep . || echo admin)"
fi

# The management API and the load tests' mock on the API site's port 8088
# (docker-compose.bench.yml), as the fields of pacod-benchmark's gateway form want them: each
# value pasted as it is. Its mock, as the gateway reaches it, is the one Caddy's /load-mock/
# route leads to (gw_load_mock in platform/caddy/gw.caddy), so that the baselines and the cells
# call the same mock.
BENCH=
case "$(env_of pacod-caddy GW_API_EXTRA)" in
    *gw_benchmark*) BENCH=$(published pacod-caddy 8088) ;;
esac
if [ -n "$BENCH" ]; then
    b="http://$(reach "$BENCH")"
    wide() { printf '  %-38s %s\n' "$1" "$2"; }
    echo ""
    echo "  pacod-benchmark on another machine (BENCH=1), its gateway form. Plain HTTP, for"
    echo "  PGW_ADMIN_ALLOW_CIDRS only; the token is a platform admin's (its runbook, section 3)."
    wide "Management API" "$b"
    wide "The mock, as the gateway reaches it" "http://gw-load-mock:9000"
    wide "The same mock, as the agents reach it" "$b/load-mock"
    wide "Data plane, as the agents reach it" "$b"
    wide "Its console" "$(sites "$CONSOLE_SITE" | head -n 1)"
fi

# Every other published port: in the development stack nearly every service, in production
# only what the hub layout offers its gateways (PostgreSQL, analytics, OTLP, Valkey).
# Each line starts with a rank, cut off after sorting, so that the most used come first. A
# function, not inline in $(...): bash 3.2 (sh on macOS) misreads a case pattern's ")" there.
other_ports() {
    for c in $(docker ps --filter "label=com.docker.compose.project=$PROJECT" --format '{{.Names}}' | sort); do
        case "$c" in pacod-caddy | pacod-grafana) continue ;; esac
        docker port "$c" 2>/dev/null | grep -v -e '-> \[' -e '/udp ' | while read -r port _ addr; do
            port=${port%/tcp}
            url="http://$(reach "$addr")"
            case "$c:$port" in
                pacod-gw:8080) rank=10 what="Data plane" ;;
                pacod-gw:8081) rank=20 what="Management API" ;;
                pacod-gw:9100 | gw-gateway-*:9100) rank=95 what="Metrics of $c" url="$url/metrics" ;;
                gw-gateway-*:8080) rank=11 what="Data plane of $c" ;;
                gw-analytics:8090) rank=30 what="Analytics" ;;
                gw-tester:8095) rank=40 what="Tester" ;;
                gw-broker:8097) rank=60 what="Broker bridge" ;;
                gw-mock:9000) rank=50 what="Mock provider" url="$url (in the stack http://gw-mock:9000)" ;;
                gw-load-agent:8096) rank=41 what="Load agent local" ;;
                gw-load-mock:9000) rank=51 what="Load tests' mock" url="$url (in the stack http://gw-load-mock:9000)" ;;
                gw-clickhouse:8123) rank=91 what="ClickHouse" ;;
                gw-valkey:*) rank=92 what="Valkey" url=$(reach "$addr") ;;
                pacod-postgres:5432) rank=90 what="PostgreSQL" url=$(reach "$addr") ;;
                pacod-otel:4318) rank=82 what="OTLP collector" ;;
                pacod-prometheus:9090) rank=80 what="Prometheus" ;;
                pacod-alertmanager:9093) rank=81 what="Alertmanager" ;;
                pacod-mailpit:8025) rank=70 what="Mailpit (every mail sent)" ;;
                pacod-id:8071) rank=85 what="pacod-id, internal API" ;;
                pacod-id:9170) rank=96 what="pacod-id, metrics" url="$url/metrics" ;;
                *) rank=99 what="$c $port" url=$(reach "$addr") ;;
            esac
            printf '%s|' "$rank"
            row "$what" "$url"
        done
    done
}
ports=$(other_ports | sort -t '|' -k 1,1n | cut -d '|' -f 2-)
[ -z "$ports" ] || printf '\n%s\n' "$ports"

echo ""
case "$AUTH" in
    oidc)
        email=$(env_of pacod-id PID_BOOTSTRAP_ADMIN_EMAIL)
        row "Sign-in" "through pacod-id (PGW_AUTH=oidc)"
        secret=PID_BOOTSTRAP_ADMIN_PASSWORD ;;
    *)
        email=$(env_of pacod-gw PGW_ADMIN_EMAIL)
        row "Sign-in" "the console's own (PGW_AUTH=local)"
        secret=PGW_ADMIN_PASSWORD ;;
esac
row "First administrator" "${email:-none set}"
if [ -n "$DEV" ]; then
    # Fixed in docker-compose.dev.yml and docker-compose.standalone.dev.yml.
    row "Password" "pacod-gw-dev"
else
    row "Password" "$secret in deploy/.env, not printed (the first sign-in's)"
fi

tls=$(env_of pacod-caddy GW_SITE_TLS)
if [ -n "$HTTPS" ]; then
    case "$tls" in
        "tls internal" | "")
            row "Certificates" "Caddy's own authority; trust its root certificate:"
            row "" "docker exec pacod-caddy cat /data/caddy/pki/authorities/local/root.crt" ;;
        *) row "Certificates" "Let's Encrypt, account ${tls#tls }" ;;
    esac
fi
upstreams=$(env_of pacod-caddy PGW_DATA_UPSTREAMS)
case "$upstreams" in *" "*) row "Gateways behind Caddy" "$upstreams" ;; esac
if mine gw-tester; then
    m=$(env_of gw-tester TESTER_MOCK_URL)
    row "Testing page" "${m:+mock mode with $m, }live mode"
    if mine gw-load-agent; then
        row "Load tests" "agents $(env_of gw-tester TESTER_LOAD_AGENTS)"
        row "" "mock target $(env_of gw-tester TESTER_LOAD_MOCK_URL)"
    fi
fi
e=$(env_of pacod-gw PGW_EGRESS_ALLOW_PRIVATE)
[ -z "$e" ] || row "Private model hosts" "$e (PGW_EGRESS_ALLOW_PRIVATE)"
e=$(env_of pacod-gw PGW_TOOL_EGRESS_ALLOW_PRIVATE)
[ -z "$e" ] || row "Private tool hosts" "$e (PGW_TOOL_EGRESS_ALLOW_PRIVATE)"
[ -n "$DEV" ] || row "Admin addresses" "${ALLOW:-127.0.0.1/32} (PGW_ADMIN_ALLOW_CIDRS)"

# What made the console unreachable once: names that browsers resolve differently, and a
# management API open to every address.
warned=
warn() {
    [ -n "$warned" ] || { echo ""; warned=1; }
    echo "  warning: $1"
}
for h in $API_SITE $CONSOLE_SITE $(env_of pacod-caddy PGW_ID_HOST); do
    case "$h" in
        *.local)
            warn "$h ends in .local, which macOS and many Linux machines look up by multicast DNS, not"
            echo "           through the network's DNS server: use another domain, such as .lan or .internal" ;;
    esac
done
if [ -n "$GRAFANA_HOST" ]; then
    case " $CONSOLE_SITE " in
        *" $GRAFANA_HOST "*) ;;
        *)
            warn "Grafana's address (GRAFANA_ROOT_URL) names $GRAFANA_HOST, not the console's host:"
            echo "           its links and its sign-in lead there" ;;
    esac
fi
# The benchmark's cells call gw-load-mock through the gateway, which refuses private hosts it
# does not list.
if [ -n "$BENCH" ]; then
    case ",$(env_of pacod-gw PGW_EGRESS_ALLOW_PRIVATE | tr -d ' ')," in
        *,gw-load-mock,*) ;;
        *) warn "gw-load-mock is not in PGW_EGRESS_ALLOW_PRIVATE: the benchmark's cells fail with an egress error" ;;
    esac
    case ",$(env_of pacod-gw PGW_TOOL_EGRESS_ALLOW_PRIVATE | tr -d ' ')," in
        *,gw-load-mock,*) ;;
        *) warn "gw-load-mock is not in PGW_TOOL_EGRESS_ALLOW_PRIVATE: the benchmark's mcp and a2a cells fail" ;;
    esac
fi
case "$(env_of pacod-caddy GW_API_EXTRA)" in
    *gw_admin_api* | *gw_benchmark*)
        case " $ALLOW " in
            *" 0.0.0.0/0 "* | *" ::/0 "*)
                warn "PGW_ADMIN_ALLOW_CIDRS admits every address to the management API on the API site:"
                echo "           list the machines that need it" ;;
        esac ;;
esac
echo ""
exit 0
