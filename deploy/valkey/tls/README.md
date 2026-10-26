# Valkey's certificate on the central host

`docker-compose.hub.yml` mounts this directory (or the one `PGW_VALKEY_TLS_DIR` names) into
`gw-valkey`, which then serves TLS on port 6380 for gateways on other machines
(docs/runbook.md, section 9). It needs two files:

| File | What |
|---|---|
| `valkey.crt` | The server certificate, with the central host's private address as an IP subject alternative name (`PGW_CLUSTER_BIND`). |
| `valkey.key` | Its private key, readable by the user Valkey runs as (999). |

Certificates and keys are not committed (`.gitignore`). With an authority of your own, for
example on the central host as root, where `10.0.0.10` is `PGW_CLUSTER_BIND`:

```sh
cd deploy/valkey/tls
openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -subj "/CN=pacod-gw valkey CA" \
  -keyout ca.key -out valkey-ca.pem
openssl req -newkey rsa:3072 -nodes -subj "/CN=gw-valkey" -keyout valkey.key -out valkey.csr
printf 'subjectAltName=IP:10.0.0.10\nextendedKeyUsage=serverAuth\n' > valkey.ext
openssl x509 -req -in valkey.csr -CA valkey-ca.pem -CAkey ca.key -CAcreateserial -days 825 \
  -extfile valkey.ext -out valkey.crt
chown 999:1000 valkey.crt valkey.key && chmod 0400 valkey.key
rm valkey.csr valkey.ext
```

Copy `valkey-ca.pem` to each gateway's `deploy/node/tls/`, and keep `ca.key` somewhere other
than this host. Restart `gw-valkey` after replacing the certificate.
