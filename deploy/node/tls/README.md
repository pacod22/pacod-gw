# Certificates of the central host

`docker-compose.yml` mounts this directory at `/run/pacod-gw-tls` in the gateway. Put the
authority of the central Valkey's certificate here as `valkey-ca.pem`
(deploy/valkey/tls/README.md); `PGW_VALKEY_CA_FILE=/run/pacod-gw-tls/valkey-ca.pem` in `.env`
names it. A certificate from an authority the image already trusts needs no file: leave
`PGW_VALKEY_CA_FILE` empty.

Certificates are not committed (`.gitignore`).
