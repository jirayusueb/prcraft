# Product

<!-- impeccable:product-schema 1 -->

## Platform

macOS desktop

## Stack

Rust on egui, hosted by the craft-ui runtime from craft-libs (see AGENTS.md). macOS only.

## Users

An individual engineer who clears a review queue every day across many repositories on both GitHub and GitLab. They review in focused blocks, usually between their own coding tasks.

## Product Purpose

PR Craft is a desktop app for reviewing pull requests (GitHub) and merge requests (GitLab) in one place. It succeeds when a reviewer gets through their queue faster and with better-quality comments than in the two web UIs.

## Positioning

- One inbox that holds GitHub PRs and GitLab MRs together.
- Keyboard-first: moving between changes, files, hunks, and comments works without a mouse.
- AI summaries of each change and suggested review comments. The reviewer stays in control: suggestions are drafts until they accept them.
- Works offline: comments are drafted locally and submitted to the forge as one batch.

## Operating Context

- macOS desktop, so native macOS conventions apply: menu bar, ⌘ shortcuts, window chrome, sidebar.
- Long reading sessions over code diffs, often next to an editor and a terminal.
- Each forge keeps its own words: GitHub "pull request / approve / request changes"; GitLab "merge request / approve / pipeline".

## Capabilities and Constraints

- Talks to GitHub through the `gh` CLI and to GitLab through the `glab` CLI, reusing the logins those CLIs already hold. The app never asks for a pasted token. Self-hosted GitLab works through whatever hosts `glab` is logged into.
- AI summaries and suggested comments run through an engine the user picks: Claude Code or Z.ai (GLM).
- Reviews stacks of dependent changes. A pull request or merge request whose base branch is another open one's head branch joins that stack; GitLab stacks made with `glab stack` show up the same way. Each layer is reviewed on its own diff (against the layer below), in order, and the inbox groups a stack into one entry.
- Notifies the reviewer about work that changed their turn: review requested (including a stack layer becoming reviewable), new commits after their review, changes requested or approval on their own changes, mentions, a failed pipeline on something they review or own, an AI summary that is ready, and queued reviews posted after reconnecting. Delivery is through macOS notifications, the Dock badge (count of reviews waiting), and an in-app list. Quiet hours and macOS Focus hold notifications without dropping them.
- Filters a change's files with include and exclude filename patterns (globs such as `internal/**`, `!*_test.go`, `!vendor/**`). Filters can be saved per repository.
- Marks files as viewed with a checkbox. A viewed file collapses and counts toward "n of m viewed"; if it changes again, it un-marks itself.
- Babysit mode, per pull or merge request the user owns: an AI agent (Claude Code or Z.ai) watches it until merge. It investigates failing pipelines and proposes fix commits, drafts replies to review comments, and rebases when the base moves. Each power is set per PR: off or ask first. A fix arrives as a patch that PR Craft applies in a local checkout and shows as a review; nothing is pushed or posted until the user does it with one explicit action. Entry points: the inbox queue "Your changes" (a Babysit card on the selected PR, key B), the review header of your own PR (key B), and ⌘K.
- Review agents: the user creates named reviewers, each with a prompt, attached skills (from the user's or the repository's skill folders), an engine, a file scope (include/exclude patterns) and triggers. An agent can split its work across subagents (per file group or per concern) and run them in parallel. Findings arrive as AI draft comments that post only when kept. The prompt editor takes slash commands: typing / opens a menu at the caret to insert a skill, a file or glob, a variable, a finding-format block, an ask-before-posting rule, a scope or a subagent split. Inserted items become inline tokens and stay in sync with the form (a /skill token is also an attached skill).
- Spawned subagents: besides the planned split, an attached skill (or the runner, e.g. to merge findings) can start more subagents during a run, and those can start their own. The run shows every subagent in one tree: planned vs spawned, who spawned it (which skill, subagent or the runner) and why (the skill's rule), its task, status, time, files read and findings, with a live activity log and Stop per subagent. Each agent sets spawn limits (how many extra, how deep, how many at once); a spawn past them is refused and logged. The engine gets the redacted diff only and has no tools.
- Activity: one list of every AI run on this Mac across all PRs (review agents, Visualize, babysit). It opens as a popover from the task pill in any title bar or from the menu-bar item, and as a full Activity window (⌥⌘J) with filters (running, looks stuck, done, failed) and the selected run's last output lines and process id. Runs are grouped by PR, with each run's subagent count, elapsed time and last output. A run with no output for 5 minutes is flagged as looking stuck; steps known to be long (tests, builds, rebases) get 15. The threshold is set in Settings → AI engine, where the app can also retry once on its own before telling the reviewer. A stuck run raises a macOS banner with Kill and retry. Running jobs offer Stop (finish the current step), there is a Stop all, and Kill ends the CLI process now. Babysit waiting on CI shows as idle by design, not stuck.
- Settings pages: Accounts, Repositories, AI engine (engine, stuck rules, runs at once, default spawn limits, usage), Privacy (what the AI engine may read, never-send patterns, secret redaction, prompt log), Review agents, Notifications (including Back to you reminders), Appearance, Accessibility (contrast, symbols with diff colour, colour-blind palette, VoiceOver announcements, focus rings, code line height), Permissions, Keyboard (every shortcut, conflicts with macOS apps flagged, rebindable), Offline and sync (poll interval, slower on battery, what to do if the PR changed while offline, queued posts with Send now / Discard).
- Window size: the smallest window is 1100 × 720. Below 1280 px wide the files pane and review panel fold to rails automatically; they keep the per-file status and the Submit button.
- Slash commands open only when / starts a word, so paths like a/b in a prompt do not trigger the menu. The same menu works in review comments, with comment commands: /suggest (a suggested-change block for the selected lines), /explain (ask the AI engine about the lines), /link, /ref, and saved replies such as /nit and /blocking.
- Merging: an approved change shows a merge card with the method (squash, merge commit, rebase), an editable commit message, delete-branch and close-issue options, the repository's merge queue (position and wait) and "Merge when ready" (auto-merge). Conflicts with the base show each conflicting file with its markers and three ways out: draft a resolution commit with the AI engine (reviewed before any push), rebase in Terminal, or open in the editor.
- Threads: Resolve thread (⇧R) and reply inline; comments whose lines changed show as Outdated with a link to the original. Hunks can expand context above and between (⌥↑, ⌥E).
- Diff options: Unified or Split (needs 1280 px), hide whitespace-only changes (W, with a count of hidden lines), highlight changed words, wrap long lines; saved per repository.
- Command palette (⌘K): actions, changes, settings and queues by name. Search covers titles, branches, comments and local drafts, grouped by kind, including merged and closed changes.
- Checks: a failed check opens its job log in PR Craft (failed lines tinted, re-run, open on the forge) with an AI explanation marked as a draft. Commits can be reviewed as all changes, since your review, or any picked set/range. A change can be checked out in a worktree or the current checkout and opened in VS Code, Xcode, Zed or Terminal (⌘O).
- Authoring: drafts show a Ready for review action with suggested reviewers (CODEOWNERS, recent authors of the files, frequent reviewers), labels, assignee, milestone and linked issue.
- First run checks gh, glab and the AI engine, shows the exact command for anything missing, re-checks on return, and lets the user start with one forge. About shows the version and updates (drafts and queued posts survive an update); crash reports are opt-in and contain no code, comments, tokens or repo names.
- Accessibility: follows macOS settings, plus increase contrast, symbols alongside diff colour, a colour-blind palette, VoiceOver announcements for new drafts, persistent focus rings and code line height. AI privacy: choose what the engine may read (diff only, diff and changed files, whole checkout), never-send patterns, automatic secret redaction, an optional prompt log, overridable per repository.
- Repository filter: clicking a repository in the Inbox sidebar filters the list to that repository across every queue (Review requested, Back to you, Your changes …), grouped by queue, with `repo:` added to the query field.
- Stacks: a stacked review can show This change or the Whole stack (W). Whole stack lists every layer against main in merge order, each with its status and an Open layer link; comments still attach to the layer that owns the line.
- Back to you, author side: on your own change, Your changes shows which reviewer requested changes, each thread with its state (waiting on you, your draft reply, resolved), and actions to draft replies with the AI engine, post replies, and ask the reviewer for another look.
- After an agent is saved, the editor reports what it will run on: the open changes waiting for the reviewer that match its scope (with files matched), the ones it skips and why, and a choice between "Run on these now" and "Only new requests". A "Where it runs" card turns the agent on or off per repository (All repos or Chosen); repos with no files matching the scope say so. The same switch is reachable from the repository side: Settings → Repositories lists, for a repo, every agent with its scope, what it matches there and a toggle, plus a general pass and the repo's saved file filter. An agent turned off for a repo is left unticked in that repo's AI review sheet, where "Off for this repo ›" links back to the agent's setting.
- AI review of a whole PR: one action ("AI review", ⌥⌘A, in the title bar of any PR screen) runs every review agent whose scope matches the changed files, plus a general pass with the default prompt for files no agent covers, so every file is reviewed. Before starting, a sheet shows the agents, a files × agents coverage map, and options: merge duplicate findings into one summary, skip files already marked viewed. With no agents yet, the sheet runs the general pass alone and offers Create an agent. Agents with "When review is requested" run this automatically. While agents run, the panel keeps a merged summary of findings so far, with duplicates folded. Findings arrive as AI drafts; nothing posts until the reviewer keeps it and submits.
- Visualize: the chosen AI engine describes a change as a template (change map, call flow, sequence, data model, risk heatmap, walkthrough) in JSON or SVG, and PR Craft draws it natively. The walkthrough template splits the change into numbered steps, each with its lines and any agent finding. There is no HTML and no script, so nothing in the artifact can reach the network, files, tokens or the CLIs. A Source tab shows the JSON or SVG, read-only; changes are made by prompting. Links inside it open lines in the diff. Every generation is kept as a version with its prompt. Artifacts are AI drafts and can be saved, or posted to the review as a PNG snapshot.
- File types: images compare as 2-up, Swipe, Onion or Difference (identical pixels black, with a threshold to ignore tiny changes); SVGs show Rendered, Structure (element and attribute changes) and Source; notebooks show changed cells with their code diff and before/after outputs, with metadata hidden, and a JSON view.
- Appearance settings: theme (light, dark, follow macOS), interface font and size, code font, size and line height, density, and code colour theme. Light and dark use the same token names.
- macOS permissions: a settings page lists the grants the app needs, each with its status and a button that opens the right System Settings pane: notifications, Keychain (Z.ai key), access to local checkouts (folders the agents read), automation of Terminal (running `claude`), launch at login, and the menu-bar item.
- Infographic: generates a shareable image of a review or a stack (review summary, release note, or stack progress) at 1200×630, square, or Slack size. It shows titles, numbers, people and checks, but never code. Copy or save as PNG.
- Menu-bar item: the PR Craft mark with the waiting count. Its menu lists what is waiting on the user, babysit items that need approval, connection status, pause notifications, sync, and open or quit.
- Back to you (re-review loop): after a review without approval, the change leaves the reviewer's count while it is with the author. It comes back as the reviewer's turn when the author answers: new commits, a reply to the reviewer's thread, all of the reviewer's threads resolved, a re-request, an @mention, or a push that dismissed the reviewer's approval. The re-review shows only the commits since the reviewer's last review, the reviewer's threads with the author's answers, and a suggested verdict. Reminders: a banner when every thread is answered, the 10:00 digest after one working day, at most one per change per day. "Hand off" reassigns the review. Solves: reviewed once, forgot, and the author had to chase for the approval.
- File views: the diff picks a view by file type. Markdown renders with word-level marks. Images get 2-up with metadata, plus Swipe, Onion skin and Difference, with comments pinned to a point. SVG renders. JSON/YAML/TOML show changed key paths. CSV shows a table with changed cells. Notebooks show cells. Lockfiles show a dependency summary. Binaries and very large files show facts only. Source is always one key away, comments anchor to source lines so they post unchanged, and the chosen view is remembered per extension.
- Failure behaviour: nothing the reviewer wrote is lost. If a post is rejected (PR closed, permissions, network), drafts and the verdict stay on this Mac and can be copied as Markdown. If an AI engine stops (usage limit, CLI missing, timeout), only AI output is missing: the app says when it can retry or offers the other engine, and partial agent results are kept with a per-subagent retry. An expired gh/glab login or a missing CLI pauses that forge's sync and shows the exact command to fix it. Very large changes load progressively; generated files are folded and left out of AI review.
- Screens in the first pass: unified inbox, diff review, account connection.
- Undecided: pricing, team features, which models each engine uses.

## Brand Commitments

- The user pinned a rebrand modelled on the "Settle" brand-guidelines references (Nov 2026): calm paper ground, graphite type, one warm signal colour, one cool "on the way" colour, out-of-focus grainy photography, dot-matrix numerals. Visual details live in DESIGN.md.
- Logomark: two equal pills, the second dropped by one radius. "Two versions of one change. PR Craft is the space where you read the difference."
- The references set type in Inter Tight, which the Figma plugin cannot load; Geist and Geist Mono stand in. Revisit if Inter Tight becomes available.

## Evidence on Hand

None. There are no real users, metrics, logos, or testimonials. All sample PRs, repos, and people in the designs are synthetic and must not be read as real.

## Product Principles

1. The diff is the work. Everything else stays out of its way.
2. One mental model across two forges, while keeping each forge's own words where they differ.
3. The keyboard is the primary input. Every action has a visible shortcut.
4. AI drafts, the human decides. A suggestion never posts without an explicit accept.
5. Offline is a normal state, not an error. Pending local work is always visible.
