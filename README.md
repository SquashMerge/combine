# Combine

Combine is one desktop window over the [herdr-core](https://github.com/herdrdev/herdr-core) agent terminals on each of your machines, plus the panels your agents build next to them: diagrams, tables, logs and views of the tools they use. It is named for the combine harvester and for the verb: it combines every agent terminal, and the views agents build around them, into one window.

A small daemon on each agent host owns the terminals, the agents' MCP server and the panel loaders. The macOS app is a client that reaches each daemon over SSH, or over a Unix socket on the same machine. The daemon opens no network port.

## Status

Early. The design is in [SPEC.md](SPEC.md). The M0 terminal-fidelity spike is done ([findings](spike/m0/FINDINGS.md)), and M1 (daemon, streaming terminals, hosts) is next ([plan](docs/m1-plan.md)). Nothing is usable yet.

## Working on it

Read [AGENTS.md](AGENTS.md) first. The architecture is enforced from the first commit: `trellis rules check` (`.trellis/rules.toml`) and `python3 -I tools/arch-guard/check.py` must both be clean.

## License

Combine is source-available under the [Business Source License 1.1](LICENSE) (`BUSL-1.1`).
You may use it in production free of charge if you are an individual, use it non-commercially
or for education or research, or are an organization (with affiliates) of no more than 5
employees and contractors, under US$5M annual revenue and under US$50M total outside funding.
Non-production use (reading, modifying, testing) is always free; anyone may also evaluate it in
production for up to 30 days. While a version is under the BSL, offering it, or a product
substantially derived from it, to third parties as a commercial hosted service or embedded
product is outside the free grant and needs a commercial license. Each version converts to the
Apache License 2.0 three years after it is first publicly distributed under BSL-1.1, and those
restrictions then end.
For commercial licensing, contact jcarreira@gmail.com. The [LICENSE](LICENSE) text governs; this
summary does not.

Third-party material is not part of the Licensed Work and keeps its own license.
