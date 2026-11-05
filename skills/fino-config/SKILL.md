---
name: fino-config
description: Set up and maintain Fino project configuration, file filters, rule directories, suppression compatibility, and violation snapshots. Use when creating or editing .fino/config.toml, integrating lint commands into hooks or CI, or migrating project policy from Credo. For authoring individual lint rules, use fino-rules instead.
---

# Fino configuration

1. Inspect existing configuration, rules, snapshot, and hook/CI commands before
   changing policy. Run `fino schema config` and `fino --help` to check the
   installed version. Preserve unrelated settings. Fino currently supports Elixir
   and TOML, not arbitrary languages or YAML.
2. Create or edit `.fino/config.toml`. File globs are relative to the scan root;
   `rules_dir` and `snapshot_file` are relative to the config file, not the shell's
   working directory. Keep exclusions narrow and explain why each is needed.

   ```toml
   include = ["**/*.ex", "**/*.exs"]
   exclude = ["**/generated/**"]
   rules_dir = "rules"
   snapshot_file = "snapshot.jsonl"
   ```

   Each rule lives in `<rules_dir>/<id>.toml`. All rules there are loaded; there
   is no config list of enabled rule ids. An empty policy fails `fino check`.
3. Validate the whole policy and its examples, then scan without writes:

   ```sh
   fino validate
   fino test
   fino check --json
   ```

   Run from the project root, or set `--root PATH` and optionally `--config FILE`.
   Explicit source paths and `--files-from FILE` contain root-relative paths.
   Unsupported/excluded paths are skipped. Full scans respect ignore files and
   skip dependency/build directories; do not assume an explicit list equals a full scan.
4. For hooks, use `fino check [FILES...]` or `fino check --files-from FILE` (one
   relative path per line). Preserve exit codes: **0** pass, **1** new error
   findings/failed examples, **2** invalid policy, read errors, or parse errors.
   Use `--json` for automation. Run a full `fino check` in CI to catch findings
   outside the changed-file set. Do not add a persistent manifest unless needed.
5. Prefer fixing findings. Use a targeted native suppression only when justified:

   ```elixir
   # fino:disable-next-line no-inspect -- temporary investigation
   IO.inspect(value)
   ```

   Also supported: `disable-line`, `disable-file`, and target `all`. Unknown
   directives/rule ids fail closed. For Credo comments, enable
   `[compat] credo_directives = true` in config and declare the exact
   `[compat] credo = "Credo.Check.Warning.IoInspect"` alias in that rule.
   Consult the config schema before defining custom `suppression_dialects`.
6. Never baseline findings just to make a check pass. With explicit approval,
   `fino snapshot` adopts current error findings; commit the resulting snapshot
   with the policy. `fino snapshot --update` removes stale entries without
   adopting new violations. Subset runs affect only selected files. Neither
   `check` nor `profile` writes snapshots. Warnings do not enter snapshots.
   Do not edit snapshot entries by hand.

Report changed policy scope, commands run, and remaining findings. Keep project
configuration separate from rule behavior; use `fino skills rules` for authoring.
See https://github.com/pdqcom/fino/blob/main/docs/configuration.md for details.
