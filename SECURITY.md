# Security Policy

QueryBit parses archives, documents, databases, and compressed data that may come from untrusted sources. Security reports are taken seriously. This is especially true for memory-safety bugs, path traversal, resource exhaustion, unsafe temporary-file handling, or malformed input that could make QueryBit report a complete no-match search when the search was actually incomplete.

## Supported versions

Only the latest released version is supported. Security fixes may ship in a new patch release instead of being backported to older releases.

## Reporting a vulnerability

Please do not open a public issue with exploit details or a proof of concept.

Use GitHub's private vulnerability reporting / Security Advisories for this repository when available. Please include the affected QueryBit version and operating system. A minimal reproducer, the expected and observed behavior, and your assessment of impact are also helpful.

If private vulnerability reporting is unavailable, open a public issue asking only for a private security contact. Do not include vulnerability details in that issue.

## What to expect

Reports will be checked for reproducibility and impact. If an issue is confirmed, the goal is to fix it before publishing technical details that would make exploitation easier. Credit will be given in release notes when appropriate and when the reporter wants it.

## Security model

QueryBit is a local command-line search tool. It does not run a background service, listen on a network port, or upload searched files as part of normal operation.

It enforces limits on recursion, extraction, parser input, and SQLite row/value size. When those limits or malformed inputs prevent a complete search, QueryBit reports the search as incomplete instead of silently returning "no match" where possible.
