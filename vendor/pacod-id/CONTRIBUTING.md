# Contributing

Thank you for your interest in pacod-id.

## Code contributions

pacod-id does not accept contributions at the moment: no pull requests, patches, documentation
changes or translations. Pull requests will not be reviewed or merged. This may change later; this
file will say so when it does.

## Bug reports and feature suggestions

Both are welcome. Open an issue in the repository's **Issues** tab. Search the existing issues
first: if yours is already there, add what you know to it instead of opening a new one.

A bug report helps most when it gives:

- the version or commit, and which part is affected: the service (sign-in, accounts,
  organisations, role bindings, the change feed, mail), the rules engine and which of its ports
  (Rust, Python, TypeScript), a client library, the hosted sign-in and account pages, or the
  deployment files (the image, the development stack, the Helm chart);
- how you run it, and the application that signs in through it;
- the steps to reproduce, and what happens compared with what should happen;
- the error messages and log lines.

A feature suggestion helps most when it says:

- what you are trying to do, and what stops you today;
- how you would expect pacod-id to behave;
- what you use instead, if anything.

Issues are public. Remove passwords, tokens, client secrets, keys and other people's personal data
from everything you post. Placeholders are enough.

## Security vulnerabilities

Do not report a vulnerability in a public issue. Use GitHub's private vulnerability reporting: the
repository's **Security** tab, then **Report a vulnerability**.
