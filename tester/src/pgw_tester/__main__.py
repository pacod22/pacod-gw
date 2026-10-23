"""`python -m pgw_tester`: serves the API on TESTER_LISTEN (default 0.0.0.0:8095)."""

import os

import uvicorn

from . import logs
from .app import create_app


def main() -> None:
    # TESTER_LOG_LEVEL, TESTER_LOG_FORMAT and SERVICE_NAME (logs.py).
    logs.setup()
    host, _, port = os.environ.get("TESTER_LISTEN", "0.0.0.0:8095").rpartition(":")
    # log_config=None: uvicorn's own messages go through the handler above instead of the
    # coloured ones it would set up. No access log: the requests are the console's.
    uvicorn.run(
        create_app(),
        host=host or "0.0.0.0",
        port=int(port),
        access_log=False,
        proxy_headers=False,
        log_config=None,
    )


if __name__ == "__main__":
    main()
