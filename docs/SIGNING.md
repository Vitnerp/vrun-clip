# Code signing plan

- **Windows**: SignPath Foundation (free for open source), described below. Status: **not started**; SmartScreen shows "More info → Run anyway".
- **macOS**: Apple Developer ID + notarization, see [macOS](#macos-apple-developer-id--notarization) at the end. Status: **not started**; the app is ad-hoc signed, so users click "Open Anyway" once.

## Windows: SignPath Foundation

We will use the free [SignPath Foundation](https://signpath.org/) program for open-source projects. The certificate is issued to *SignPath Foundation*, which is shown as the publisher. SignPath signs only artifacts that were built by GitHub Actions from this public repository.

## Prerequisites (check before applying)

- [x] Public GitHub repository: `Vitnerp/vrun-clip`
- [x] OSI-approved license (MIT, see `LICENSE`), no dual licensing
- [x] Automated builds in GitHub Actions (`.github/workflows/release.yml`)
- [ ] At least one published release (`v0.1.x`)
- [ ] MFA enabled on GitHub (and later on SignPath) for everyone with write access
- [ ] A "Code signing policy" section in README (template below)

## Steps

1. **Apply** at https://signpath.org/ (application form for OSS projects). Describe the project, link the repo and releases. Wait for review.
2. **After approval**, SignPath creates an organization and project. Note:
   - Organization ID
   - Project slug (e.g. `vrun-clip`)
   - Signing policy slug (`release-signing`, maybe `test-signing`)
   - Artifact configuration (which files to sign: the app exe and the NSIS setup exe)
3. Install the **SignPath GitHub App** on the repository (lets SignPath verify that artifacts come from our workflow).
4. Create a SignPath **API token** (CI user, submitter role) and store it as the GitHub secret `SIGNPATH_API_TOKEN`.
5. **Change the release workflow** to sign before publishing. Two signing passes are needed, because the exe inside the installer must be signed too:
   1. Build the app without bundling: `pnpm tauri build --no-bundle`, then upload `vrun-clip.exe` with `actions/upload-artifact`.
   2. `signpath/github-action-submit-signing-request@v1` (`wait-for-completion: true`), then download the signed exe back into `src-tauri/target/release/`.
   3. Bundle the NSIS installer from the signed exe: `pnpm tauri bundle --bundles nsis`.
   4. Upload the setup exe and sign it with SignPath the same way.
   5. **Re-create the updater signature** for the *signed* installer: `pnpm tauri signer sign <setup.exe>` (uses `TAURI_SIGNING_PRIVATE_KEY`), then write `latest.json` (version, notes, pub_date, `platforms.windows-x86_64.{signature,url}`).
   6. Create the GitHub Release with the signed setup exe, its `.sig` and `latest.json` (e.g. `softprops/action-gh-release`).

   Signing changes the file bytes, so the updater `.sig` must always be made **after** Authenticode signing.
6. Add the required attribution to the release notes and README:
   > Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org)

## README "Code signing policy" template

```markdown
## Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

- Committers and reviewers: [Vitnerp](https://github.com/Vitnerp)
- Approvers: [Vitnerp](https://github.com/Vitnerp)

Only release builds produced by this repository's GitHub Actions are signed.

Privacy: vrun.clip never sends clipboard content anywhere. Its only own network request is the update check to GitHub Releases.
```

## Notes

- SmartScreen reputation builds up per certificate over time. The first signed releases may still show a (milder) warning until enough people have downloaded them.
- Paid alternatives, if ever needed: a Certum "Open Source Developer" certificate (about €69/year, publisher = your name), or Azure Artifact Signing (about $10/month; individuals only in the USA and Canada, EU *organizations* are eligible).

## macOS: Apple Developer ID + notarization

SignPath does not cover macOS. Apple requires its own program:

1. Join the **Apple Developer Program** (99 USD/year, individuals allowed, needs an Apple ID with 2FA).
2. On a Mac, create a **Developer ID Application** certificate (Xcode → Settings → Accounts, or developer.apple.com), export it with its private key as `.p12` and a password.
3. Create an **app-specific password** for your Apple ID (account.apple.com), and note your **Team ID**.
4. Add GitHub secrets: `APPLE_CERTIFICATE` (base64 of the `.p12`), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` (e.g. `Developer ID Application: Your Name (TEAMID)`), `APPLE_ID`, `APPLE_PASSWORD` (app-specific password), `APPLE_TEAM_ID`.
5. In `.github/workflows/release.yml` pass those secrets as `env` to `tauri-action`, and remove `"signingIdentity": "-"` from `src-tauri/tauri.macos.conf.json`. Tauri then signs and notarizes automatically.

Benefits: no "Open Anyway" step, and the Accessibility permission survives updates (it is tied to the signing identity; ad-hoc signatures change on every build).
