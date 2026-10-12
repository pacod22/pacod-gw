"""What a Python product needs from pacod-id.

- ``pacod_id.authz``: the rules engine.
- ``pacod_id.client``: the internal API with the service's client credentials.
- ``pacod_id.grant``: delegated grants for a person's long task (the gateway's tester).
- ``pacod_id.verify``: checks of the access tokens the product's API receives.
- ``pacod_id.cache``: the product's copy of the bindings, following the feed.
- ``pacod_id.fastapi``: FastAPI dependencies (the ``fastapi`` extra).
"""

__version__ = "0.1.0"
