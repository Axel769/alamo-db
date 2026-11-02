# Security policy

Alamo DB connects to databases with real credentials, often in production.
Security reports are taken seriously and handled privately.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private reporting instead:
[Report a vulnerability](https://github.com/Axel769/alamo-db/security/advisories/new)
(repository → Security → Report a vulnerability).

Please include:

- the affected version (release tag, e.g. `v0.1.0-alpha.12`);
- operating system and database engine/version, if relevant;
- steps to reproduce and the impact you observed.

You will get an acknowledgement within 7 days. A fix is released as a new
version through the in-app updater, and the report is credited in the
advisory unless you ask otherwise.

## Supported versions

Alamo DB is in alpha: only the latest release receives security fixes.

## Scope

In scope, for example: credentials written to disk or logs, TLS or SSH host
verification that can be bypassed, SQL executed without the user's action,
update packages accepted without a valid signature.

Out of scope: issues that need an already compromised machine or user
account, and vulnerabilities of the database servers themselves.
