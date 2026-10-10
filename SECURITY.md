# Security policy

pacod-gw holds every provider key of an installation and sees every prompt that passes
through it. Reports of weaknesses are welcome and are handled privately until a fix is out.

## Reporting a vulnerability

Do not open a public issue. Write to:

**`<security contact: to be filled in by the maintainers before the first release>`**

Please include:

- the version or commit, and which part is affected: the core (data plane or management
  API), the analytics service, the console, the tester, or the deployment files;
- what an attacker needs (a gateway key, a console role, network access, nothing);
- the steps to reproduce, and what happens compared with what should happen;
- whether you have told anyone else.

Never send real provider keys, gateway keys or prompts that are not yours. Placeholders are
enough.

## What happens next

The maintainers confirm that the report arrived, agree on a disclosure date with you, and
tell you when the fix is released. You are credited in the release notes unless you prefer
not to be.

Response times: `<to be filled in by the maintainers>`.

## Supported versions

Until version 1.0, only the latest release receives security fixes.

## Scope

In scope: the code in this repository and the deployment files it ships (`deploy/`,
including the Helm chart), with their defaults.

Out of scope: the providers behind the gateway, and settings an operator chose against the
documentation, such as an admin network allow-list open to everyone. Defaults that make
such a mistake easy are in scope.

What the gateway protects against, and how, is described in
[docs/threat-model.md](docs/threat-model.md).
