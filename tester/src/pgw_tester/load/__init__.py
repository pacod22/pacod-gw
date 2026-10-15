"""Load tests: the tester sets up what a load test needs, drives the load agents
(`pacod-gw-load-agent`) that make the calls, adds up what they report, and cleans up.

The agents speak the JSON contract of `crates/pacod-gw-load/src/contract.rs`; `contract.py`
mirrors it, and both sides are tested against the fixtures in
`crates/pacod-gw-load/tests/contract/`.
"""
