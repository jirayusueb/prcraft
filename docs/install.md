# Install PR Craft

PR Craft is a macOS app (macOS 13 or later). The build is ad-hoc signed, not notarized, so macOS asks you to approve it once.

## Prerequisites

- `gh` and/or `glab`, signed in (`gh auth login`, `glab auth login`). PR Craft reads and posts through them; it stores no forge token.
- Optional: the `claude` CLI, for AI summaries and review agents.

## Install

1. Download `PR-Craft-<version>-macos-arm64.zip` (Apple silicon) or `…-universal.zip` (Intel) from the [latest Release](https://github.com/jirayusueb/prcraft/releases/latest).
2. Unzip it and move `PR Craft.app` to Applications.
3. Open it. On macOS Sequoia the first open is blocked: open System Settings → Privacy & Security, scroll to the message about PR Craft and choose **Open Anyway**.
4. If that button never appears, remove the quarantine mark and open the app again:

   ```sh
   xattr -dr com.apple.quarantine "/Applications/PR Craft.app"
   ```

## Update

PR Craft never replaces itself. Settings → About → **Check for updates** says whether a newer version exists; **Download** saves the zip to Downloads after checking it against the Release's `SHA256SUMS.txt`. Then quit PR Craft, unzip, and replace the app in Applications. You can also download the zip from the Release page.

## Go back to the previous version

Download the previous Release's zip the same way, quit PR Craft, and replace the app. Drafts and settings stay: they live in `~/Library/Application Support/PR Craft`, outside the app.

## Diagnostics

Settings → Offline and sync → **Copy diagnostics** copies the version, macOS, forge sign-ins and the last log lines, with secrets masked, to paste into a report. Settings → About shows the version and commit.
