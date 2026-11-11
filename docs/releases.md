# Releases

## Before enabling public releases

A passing local workflow lint is not proof that publication has been tested:
the first complete three-platform release remains to be verified.

Before making the repository public:

1. Include dependency licenses and notices in distributed archives. Refresh
   `THIRD_PARTY_NOTICES` when dependencies change; it covers the locked dependency
   tree, including transitive dependencies and vendored code.
2. Review the exact staged export and run `mise run secrets:files`.
3. Commit with signing enabled. Run `mise run secrets:history` over the entire
   resulting history before pushing. Resolve every finding; do not add blanket
   allowlists or disable detection to pass.
4. Install the release GitHub App on `pdq/fino` and grant it Contents, Issues, and
   Pull requests write access. Make the `CONNECT_RELEASE_CLIENT_ID` Actions variable
   and `CONNECT_RELEASE_PRIVATE_KEY` Actions secret available to this repository.
   The workflow requests only these permissions for the current repository and
   revokes the token after the job.
5. Make the repository public only after the audit. Private-repository GitHub
   artifact attestations require Enterprise Cloud; never disable attestations as
   a workaround.
6. Require `Checks`, `PR title`, and all three `Build (TARGET)` status checks on `main`, with strict
   up-to-date checks. Free-plan private repositories cannot enforce this protection;
   apply it after the visibility change. Do not mistake workflow checks for branch
   protection. No workflow auto-approves or auto-merges PRs.

Immutable releases are enabled in repository settings. Keep them enabled. Reading
that settings API needs administration permission, which the workflow deliberately
does not have. An administrator must verify the setting; the workflow verifies the
published release's `immutable` field and fails if it is not true.

## Release flow

Conventional PR titles become squash-merge commit messages and drive
release-please's release PRs. Individual commit messages are not linted. The manifest starts at
`0.0.0`; the initial feature commit is intended to produce `v0.1.0`. Merging a
release PR creates a **draft**, never a published release directly.

The same workflow then:

1. Checks out the exact release source commit.
2. Tests and builds native macOS ARM64, Linux ARM64, and Linux AMD64 executables.
   Linux uses musl for portable, static binaries.
3. Packages each target and creates GitHub build-provenance attestations.
4. Uploads all three archives to the draft release.
5. Signs a Packslip manifest in a separate job with no release-write permission.
   `attest: link` preserves the build jobs' provenance.
6. Verifies every archive's GitHub attestation, uploads the signed manifest, and
   only then publishes and checks that GitHub has made the release immutable.

All external actions are pinned to full release commit SHAs. The Packslip action
downloads the CLI associated with its pinned release and verifies it. Packslip
signing uses GitHub OIDC, not the release App's private key. Release write access
is separate from Packslip signing.

Release-please uses a short-lived GitHub App installation token, so its PRs trigger
CI normally. It does not rely on the setting that lets `GITHUB_TOKEN` create PRs
or on a manual CI dispatch. Build, attestation, and publication jobs keep their
separate `GITHUB_TOKEN` permissions. On a failed build/sign/publish attempt, keep
the release a draft and rerun the failed jobs. Never delete and recreate a
published release or move its tag.

## Consume a verified release

After the first release is published and verified, install an exact version:

```toml
[tools]
"packslip:github.com/pdq/fino" = "0.1.0"
```

This is a future installation example, not a claim that `0.1.0` is published.
The mise Packslip backend verifies the signed manifest, signer identity, artifact
digest, and size before unpacking. Commit the consuming project's mise lockfile.
After verifying the first release, publish its Packslip signer fingerprint here
and in the installation instructions. Never invent a fingerprint before a release
has actually been signed and verified.
