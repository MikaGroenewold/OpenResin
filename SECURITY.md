# Security policy

## Supported versions

Security fixes are made for the latest release and the `main` branch.

## Reporting a vulnerability

Please **do not** report security problems in public issues.

Report them privately through GitHub:
[**Report a vulnerability**](https://github.com/MikaGroenewold/OpenResin/security/advisories/new).

Please include:

- what is affected (app, CLI, a file format reader, a network protocol, the
  camera relay, …) and the version or commit,
- steps or a file that reproduces the problem,
- what an attacker could achieve.

You will get an answer as soon as possible, usually within a week. Once a fix
is released, the advisory is published and you are credited unless you prefer
otherwise.

## Scope

Things that are in scope include:

- crashes, hangs or memory exhaustion caused by crafted model, project or
  printer files,
- anything that lets another device on the network make OpenResin read or
  write files, run code or reach the internet,
- the local camera relay being reachable from other processes or machines
  without its access token.

OpenResin talks to printers over their unencrypted LAN protocols (for example
SDCP over plain WebSocket/HTTP). Weaknesses of those printer protocols
themselves are out of scope, but issues in how OpenResin handles them are in
scope.
