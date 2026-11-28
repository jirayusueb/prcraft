# Live-verified ledger

Each row records one write command run once against a real forge, on a sandbox change, with the user's approval at the time it ran. Tests use fixtures; only this file says what has worked live. Design: [designs/long-term-roadmap.md](designs/long-term-roadmap.md) (G6, D13).

`cargo xtask release` (H2) refuses unless the three GitHub rows for `review.submit`, `thread.reply` and `thread.resolve` are `pass`, no row is `fail`, and the latest weekly macOS build is green. A GitLab row may be `blocked: <reason>`; then the Release notes and About say "GitLab writes not verified".

Result is one of `pass`, `fail`, `blocked: <reason>`, or empty (not run). "How run" names the command and its parameters without tokens or comment text.

| Command | Forge | Host | Milestone | Sandbox change | Date | Result | How run |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `review.submit` | GitHub | github.com | M2 | | | | |
| `thread.reply` | GitHub | github.com | M2 | | | | |
| `thread.resolve` | GitHub | github.com | M2 | | | | |
| `draft.add` on a rendered view, then `review.submit` | GitHub | github.com | M5 | | | | |
| `review.hand_off` | GitHub | github.com | M4 | | | | |
| `review.submit` | GitLab | gitlab.com | M2 | | | | |
| `thread.reply` | GitLab | gitlab.com | M2 | | | | |
| `thread.resolve` | GitLab | gitlab.com | M2 | | | | |
| `draft.add` on a rendered view, then `review.submit` | GitLab | gitlab.com | M5 | | | | |
| `review.hand_off` | GitLab | gitlab.com | M4 | | | | |
