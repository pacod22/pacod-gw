"""pgw-tester: a small engine that runs declarative test cases against a running gateway.

The console starts runs on behalf of a platform admin; the engine calls the gateway's public
interfaces only (the management API with the admin's session, the data plane with a scratch
API key) and reports what it saw.
"""
