# Contributing

Thank you for your interest in pacod-gw.

## Code contributions

pacod-gw does not accept contributions at the moment: no pull requests, patches, documentation
changes or translations. Pull requests will not be reviewed or merged. This may change later; this
file will say so when it does.

## Bug reports and feature suggestions

Both are welcome. Open an issue in the repository's **Issues** tab. Search the existing issues
first: if yours is already there, add what you know to it instead of opening a new one.

A bug report helps most when it gives:

- the version or commit, and which part is affected: the core (data plane or management API), the
  minimal gateway (`pacod-gw-lite`), the analytics service, the console, the tester, or the
  deployment files;
- how you run it: the minimal gateway, Docker Compose or the Helm chart, and the providers
  involved;
- the steps to reproduce, and what happens compared with what should happen;
- the error messages and log lines.

A feature suggestion helps most when it says:

- what you are trying to do, and what stops you today;
- how you would expect the gateway to behave;
- what you use instead, if anything.

Issues are public. Remove provider keys, gateway keys, tokens, passwords and prompts that are not
yours from everything you post. Placeholders are enough.

## Security vulnerabilities

Do not report a vulnerability in a public issue. Follow [SECURITY.md](SECURITY.md) instead.
