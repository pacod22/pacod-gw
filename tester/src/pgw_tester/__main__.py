"""`python -m pgw_tester`: serves the API on TESTER_LISTEN (default 0.0.0.0:8095)."""

import logging
import os

import uvicorn

from .app import create_app


def main() -> None:
    logging.basicConfig(
        level=os.environ.get("TESTER_LOG_LEVEL", "INFO"), format="%(levelname)s %(name)s %(message)s"
    )
    host, _, port = os.environ.get("TESTER_LISTEN", "0.0.0.0:8095").rpartition(":")
    uvicorn.run(create_app(), host=host or "0.0.0.0", port=int(port), access_log=False, proxy_headers=False)


if __name__ == "__main__":
    main()
