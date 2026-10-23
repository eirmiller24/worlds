# Security

Please report vulnerabilities privately through GitHub's
[security advisories](https://github.com/eirmiller24/worlds/security/advisories/new),
not as public issues. You'll get a reply there; once a fix is out, the
advisory is published with credit unless you'd rather stay anonymous.

Worth knowing when assessing impact:

- The live API (`worlds-server`, and the editor while a project is open)
  listens on `127.0.0.1` only, requires the per-session token from
  `<project>/.worlds/server.json` (written with mode 0600), and refuses
  connections that carry an `Origin` header, so web pages can't reach it.
- Anything that can read that token, or that you give MCP access to, can
  edit the open project. That is the point of the API; treat agent access
  like any other write access to your files.
- Project files are data: loading, validating or baking a project never
  runs code from it.

Only the latest release (or `main`) gets security fixes.
