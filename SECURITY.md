# Security policy

## Reporting a vulnerability

Please report security issues privately by email to **hello@probnode.com**, with "SECURITY" in
the subject. Do not open a public issue for a vulnerability.

Include what you can: the affected version, a description, and an input file or steps that
reproduce it. We acknowledge reports within three working days and keep you informed until the
issue is fixed. We credit reporters in the release notes unless they prefer not to be named.

## Scope

Probe readers parse files that may come from untrusted sources. In scope:

- a panic, abort, unbounded allocation or hang on any input file;
- memory unsafety in any crate of this repository or in the Python extension;
- a decode that silently returns wrong data for a file that should be refused.

The readers memory-map files: a file modified by another process while it is open is outside the
threat model.

## Supported versions

Security fixes go to the latest released version of each package.
