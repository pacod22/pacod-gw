Put the PEM certificates of a private CA here when the gateway's TLS certificate comes from one,
and set `LOAD_AGENT_CA_FILE_IN_CONTAINER` in `.env` to `/etc/pacod-gw/ca/<file>`. The directory is
mounted read-only into the agent.
