---
name: PR Craft
description: Calm desktop code review for GitHub and GitLab: one number that matters, everything else quiet.
colors:
  graphite: "#1D1C21"
  paper: "#F3F1EC"
  panel: "#FBFAF8"
  raised: "#FFFFFF"
  sunken: "#F6F4F0"
  hover: "#ECE9E3"
  border: "#E4E1DA"
  border-strong: "#D8D4CC"
  text-secondary: "#4D4B48"
  text-tertiary: "#64615C"
  ember: "#F0603A"
  ember-text: "#A03000"
  ember-subtle: "#FFE7DF"
  haze: "#C2BCEF"
  haze-subtle: "#ECEAF9"
  haze-text: "#453E6F"
  moss: "#31A667"
  moss-bg: "#EEFAF2"
  moss-text: "#006F3D"
  rose: "#E94E6E"
  rose-bg: "#FFF4F4"
  rose-text: "#A81741"
  night-canvas: "#111114"
  night-panel: "#1B1B20"
  night-text: "#E9E7EE"
typography:
  statement:
    fontFamily: "Geist, system-ui, sans-serif"
    fontSize: "56px"
    fontWeight: 500
    lineHeight: "60px"
    letterSpacing: "-0.03em"
  title-large:
    fontFamily: "Geist, system-ui, sans-serif"
    fontSize: "30px"
    fontWeight: 500
    lineHeight: "36px"
    letterSpacing: "-0.02em"
  title:
    fontFamily: "Geist, system-ui, sans-serif"
    fontSize: "18px"
    fontWeight: 500
    lineHeight: "24px"
    letterSpacing: "-0.01em"
  body:
    fontFamily: "Geist, system-ui, sans-serif"
    fontSize: "13.5px"
    fontWeight: 400
    lineHeight: "20px"
    letterSpacing: "-0.005em"
  reading:
    fontFamily: "Geist, system-ui, sans-serif"
    fontSize: "14.5px"
    fontWeight: 400
    lineHeight: "23px"
  code:
    fontFamily: "Geist Mono, ui-monospace, monospace"
    fontSize: "12.5px"
    fontWeight: 400
    lineHeight: "20px"
rounded:
  key: "4px"
  control: "8px"
  window: "10px"
  pill: "999px"
spacing:
  1: "4px"
  2: "8px"
  3: "12px"
  4: "16px"
  5: "20px"
  6: "24px"
  8: "32px"
components:
  button-primary:
    backgroundColor: "{colors.graphite}"
    textColor: "{colors.raised}"
    rounded: "{rounded.pill}"
    height: "28px"
    padding: "0 12px"
  button-secondary:
    backgroundColor: "{colors.raised}"
    textColor: "{colors.graphite}"
    rounded: "{rounded.pill}"
    height: "28px"
  tag-your-turn:
    backgroundColor: "{colors.ember-subtle}"
    textColor: "{colors.ember-text}"
    rounded: "{rounded.pill}"
    height: "20px"
  tag-on-the-way:
    backgroundColor: "{colors.haze-subtle}"
    textColor: "{colors.haze-text}"
    rounded: "{rounded.pill}"
    height: "20px"
  nav-item-selected:
    backgroundColor: "{colors.graphite}"
    textColor: "{colors.raised}"
    rounded: "{rounded.control}"
    height: "28px"
  comment:
    backgroundColor: "{colors.raised}"
    rounded: "{rounded.control}"
---

# Design System: PR Craft

## Overview

A calm brand for a busy job. Paper ground, graphite type, soft raised cards, and grainy out-of-focus "photographs" carry the identity; the work itself (diffs, queues, comments) stays plain and legible. The brand idea: two versions of one change, and PR Craft is the space where you read the difference. The full token set, including dark values and syntax, is in `.omp/skills/figma-pr-craft/tokens.json` and in the Figma variables `Primitives`, `Theme`, `Spacing` and `Radius`.

## Colors

### Primary
- **Graphite** (#1D1C21): all type, primary pills, the logomark.

### Secondary
- **Ember** (#F0603A): work waiting on you. It appears only as tags, dots, the waiting bar and the unread marker.

### Tertiary
- **Haze** (#C2BCEF, subtle #ECEAF9): work on the way. AI drafts, offline-queued reviews, running pipelines.

### Neutral
- **Paper** (#F3F1EC): window canvas. **Panel** (#FBFAF8) and **Raised** (#FFFFFF): cards and inputs. **Night** surfaces (#111114 to #2E2E36) for dark.

### Named Rules
- **Ember is a state, never a fill.** No Ember buttons, no Ember headings.
- **Diff colours are Moss and Rose**, so they never collide with Ember's meaning.
- **One theme name, two values.** Every theme token exists as `light/…` and `dark/…`; every text pair passes 4.5:1 in both, syntax included.
- **Never colour alone.** Deleted words carry a strike-through as well as Rose. Ember badges with white numerals sit on Ember/600, not Ember/500 (5.1:1). No text below 11px except avatar initials.

## Typography

Geist for every word; Geist Mono for code, refs, paths, hex values and diffstat. Medium weight for titles, tight tracking (−0.5% to −3%), never italics, never all-caps labels.

### Hierarchy
Statement 56 → Display 40 → Title Large 30 → Title 18 → Heading 14 SemiBold → Body 13.5 → Reading 14.5 (summaries, comments) → Small 12.5 → Caption 11.5. Code is 12.5 on a 20px row.

### Named Rules
- **One number per surface goes dot matrix.** It's the count that matters most (reviews waiting, sources connected): lit dots for the value, faint dots for the grid.

## Layout

Dense desktop windows of 1440×900, chosen over an airy card layout on user feedback: fixed panes use the space better. The window has a 38px title bar (traffic lights, mark, breadcrumb, sync chip, avatar) and a 26px status bar (gh, glab, AI engine, command palette). Panes are split by 1px seams: a 240–260px sidebar on the hover tone, the list on the canvas, and the reading pane on the panel tone. Queue rows are 66px, nav items 28px, insets 16–20px. The diff uses fixed gutters: line numbers 0–86, marker 92, code 110.

## Elevation & Depth

Flat. Hierarchy comes from pane tone and 1px seams (#E4E1DA), never from card shadows. Only window-level surfaces cast a shadow: the window itself (0/24/60 at 20%), and popovers and macOS banners (0/12/32 at 14%). A selected row turns raised white, with no lift.

## Shapes

Pills for buttons (28px; 26px inside comments) and tags (20px). Radius 8 for fields, nav items, callouts and comments, 10 for the window and option rows, 4 for keys. The logomark is two 9×18-unit pills, the head dropped by one radius (4.5) and spaced 2 units apart.

## Components

- **Waiting block**: at the top of the inbox sidebar (224×92), the count in dot matrix on the Ember-night field, with a reviewed / drafted / waiting bar.
- **Queue row**, **AI summary panel** (Haze), **"Worth a look" note** (Ember subtle), **extent bars**, **flat comments** (local draft, AI suggestion with Keep / Edit / Discard), **CLI rows** for `gh` and `glab` (connected or missing, with the install command), **AI engine options** (Claude Code, Z.ai), **segmented filter**, **sidebar nav**, **keys**, **verdict options**, **offline callout**, **title bar and status bar**.
- **Comment on code**: selected lines in Haze (Haze/100 row, Haze/200 gutter, 2px Haze/600 rail), an inline graphite selection toolbar, and an inline composer with Write/Preview, a "Suggested change" block, and "Add to review ⌘↵".
- **Review stack**: a stack rail (layers top-first on a Stone/300 line, nodes coloured by state, base branch as a small graphite square), an inbox stack group (header with layer bars and "Your turn on n of m", one rail line per layer), and an editor strip (base → top pills, This change / Whole stack, ⌥↑/⌥↓).
- **Notifications**: a title-bar control ("3 new" with an Ember dot; graphite while open), a popover list (Today/Earlier; Ember unread dot for your turn, Haze for the rest; inline Open review), toggles (graphite on, Stone/300 off), Settings → Notifications (an events table with Banner and Dock badge columns, delivery, quiet hours, Focus), and macOS banners showing titles only, with one Open action. The Dock badge counts reviews waiting.
- **Files**: a viewed checkbox (graphite with a drawn check), Include / Exclude glob fields (Rose exclude chips) with a hidden-files count, file rows grouped under folder labels, and file headers that collapse when viewed.
- **Babysit**: an activity timeline (Ember = waits for you), a proposed-commit bar in Haze, and per-power Off / Ask / Auto controls (Auto in graphite).
- **Review agents**: agent list items, skill chips showing their source folder, a prompt field in mono with Haze variables, a subagents card (split, parallel limit, plan tree), and subagent rows with progress or finding counts. Slash commands: / opens a 560px popover at the caret, with grouped commands on the left (Insert, Shape the output, Scope and subagents; the selected one in graphite, › for a sub-list) and a sunken preview of choices on the right, plus a key legend (↑↓, ↵, Tab, Esc). Inserted items become Haze inline tokens (`/skill go-concurrency`, `/file internal/**/*.go`).
- **Integration marks**: official GitHub, GitLab, Claude and Z.ai marks name the forge or engine (forge badges, `gh`/`glab` rows, engine options, status bar). GitHub and Z.ai: ink on light, white on graphite. GitLab: orange. Claude: clay #D97757. Sources are in `.omp/skills/figma-pr-craft/assets/logos`.
- **Scrolling**: panes scroll on their own. Sticky headers (a compact 36px PR header, file 34, hunk 26; list titles and the current group) get a 1px seam once content passes under them, never a shadow. The diff has a 14px overview ruler that doubles as the scrollbar; other panes use an overlay thumb (6px at 25%, 8px at 45% on hover). New content never moves the viewport: a graphite "n new commits · position kept ⌘R" pill offers the jump. Lists end with a sentence and "Show older", never a spinner.
- **Visualize artifact**: generated HTML shown in a sandboxed web view. A toolbar holds the file name, the AI-draft chip, Preview/HTML, Open in browser, Save .html and Snapshot to review. The left pane has the prompt, template chips and the version list; the right pane has facts, sandbox rules and findings shown. Inside the artifact, a PR Craft stylesheet keeps generated pages on brand: a mono eyebrow, a 24px title, a lede, then diagrams on a dot-grid figure. Nodes are Moss when added, Graphite when modified and Stone when unchanged; Ember finding counts; links show a graphite "Open … in the diff ↵" tooltip.
- **Settings rows**: title and one-line purpose on the left, a segmented control, select, toggle or button on the right, and a hairline between rows. Theme choice is three cards drawn from the light and dark tokens.
- **Permission rows**: a System Settings tile, purpose, status dot and a button that deep-links to System Settings. A blocking missing grant becomes an Ember-subtle banner.
- **Share image**: 1200×630 on an Ember-night field with white type, one Ember dot-matrix number and a stats row. It never includes code.
- **Menu-bar menu**: native translucent menu with accent-blue highlight. PR Craft content (Ember unread dots, babysit card, agent progress) sits inside native structure.
- **Back to you**: a queue for changes you reviewed that came back. Rows read "your last review → what changed since" and are grouped into Ready to approve, Needs a look, and With the author. Rows with the author are grey, have no Ember dot and don't count. The detail view has a "since your review" timeline and thread recaps (your note, their answer, Fixed/Answered chip). The re-review diff is scoped to new commits, with a checklist of what you asked for and Approve suggested.
- **States**: loading keeps the real layout as Stone/200 skeletons and states what is loading and how far, with a 3px progress bar. AI work shows a Haze-bordered card with the engine mark, a done / now / next step list, the elapsed time and Stop. Errors and warnings are banners on raised paper with a 1px hairline and no side bar; the coloured round glyph and the coloured title carry the kind: Rose for errors, Ember when it needs you, Haze for info. The banner says what happened, what is safe (drafts kept, nothing posted) and gives one next action. Empty lists get a sentence and where to go next. There are no bare spinners.
- **Folding panes**: each window has title-bar pane toggles drawn like SF sidebar.left/right; the glyph's pane side is filled while the pane is open. Left: ⌃⌘S. Right: ⌥⌘0. The Inbox sidebar hides completely, and the list header turns into a queue switcher. The files pane folds to a 44px rail with one square per file. The review panel folds to a 44px rail that keeps the draft count and Submit. Sidebar sections fold with a chevron and show their count when folded. Dividers get a Haze bar and a width tooltip on hover; double-click resets.
- **Tooltips**: graphite bubble (bg/inverse, r 8, pop shadow) with a 12.5 title, up to two lines of 11.5 body and optional keycaps; a stepped notch points at the target. They appear after 500 ms, then instantly while you move between neighbours, and on keyboard focus; Escape hides them. They add detail: folded file names, what a count is made of, why a control is disabled, which limit stopped something, full truncated text. Never the only copy of something you need.
- **Activity**: a 560px popover under the title-bar task pill. Runs are grouped by PR (forge mark + mono ref). Each row has a status dot (running Haze, done Moss, idle ring), name, detail (subagent count, last output), elapsed time in mono, and one or two small actions. A stuck run tints the row Ember-subtle, puts its detail in signal text, and offers Kill (danger) and Retry. The pill itself turns Ember-subtle while anything is stuck.
- **Activity window (⌥⌘J)**: the full version of the popover, in three panes. Left: filters with counts. Middle: runs grouped by PR. Right: the selected run's facts in mono (engine, pid, start, last output), a warn banner when it looks stuck, its last log lines, and Kill and retry. A stuck run also raises a macOS banner with Kill and retry / Open.
- **Window size**: minimum 1100 × 720. Below 1280 px both side panes fold to their 44px rails and the diff takes the rest. The title bar keeps the back button, the breadcrumb, the PR actions and both pane toggles.
- **File view (tree or list)**: the Files pane switches between Tree and List with T. Tree compacts single-child folders, indents 14px with hairline guides, and shows viewed counts on folders. List puts the file name over its folder and sorts by path, changes, or review order. Both show Ember finding dots, Haze comment counts, and the diffstat until the file is viewed.
- **Per-type file views**: the file header carries a renderer switch (Rendered · Source; 2-up · Swipe · Onion · Difference; Rendered · Structure · Source for SVG; Cells · JSON for notebooks). Rendered markdown marks changed blocks with a 3px Moss/Rose gutter bar and changed words on the diff gutter tints. Images sit on a checkerboard, with a metadata table and Haze comment pins; Difference shows one square where black means identical, plus a threshold select. SVG Structure lists element/attribute changes as Rose old → Moss new mono chips. Notebook cells are raised cards (head: In [n] → In [m], cell kind chip) holding the code diff and a 2-up of outputs. Binaries show facts, never a fake diff.
- **Whole stack**: one diff with a 32px `bg/hover` band per layer (stack node, ref, title, status chip, Open layer), the path in tertiary mono, then that layer's lines.
- **Dark appearance**: the same layout with `dark/*` tokens: Night canvas and panels, Graphite-inverse buttons become light pills with dark text, white GitHub and Z.ai marks, diff tints composited on Night/850. Fields, the waiting block and brand marks keep their colours.
- **Merge card**: a 1.5px Graphite-outlined raised card (like the babysit card) holding the method segment, a sunken mono commit message, checkboxes, a Haze merge-queue note and the auto-merge toggle; the primary pill says what happens ("Add to merge queue").
- **Split diff**: two 438px halves split by a 2px seam; each half has its own 44px tinted gutter; missing lines are `bg/sunken`.
- **Command palette**: 640px raised sheet at y 120 with a 15px query row, grouped results (selected row graphite-inverse) and a sunken key legend. Popovers anchored to the title bar or header (Diff options, commits, Open locally) use the same raised paper, r 10, `POP_SHADOW`.
- **Expand bars**: 26px Haze-subtle rows with ↕, the hidden line range in mono and its key.
- **Interaction states** (`DS · Interaction states`): one ramp. Hover steps the paper up once (`bg/hover`), pressed twice (Stone/150); a graphite pill lifts to Stone/800 on hover and settles to Stone/900 pressed. Keyboard focus is a 2px `focus/ring` with a 2px gap, shown for keyboard moves only unless Accessibility asks; pointer hover and keyboard focus can show on two different rows at once. Selected stays graphite-inverse (sidebar, menus) or raised paper (queue rows). Disabled drops to tertiary text and takes no hover or ring. A hovered queue row swaps its status line for Open ↵ / On GitHub ⌥↵; a hovered diff line darkens to its gutter tint and shows an 18px Haze comment button in the gutter, and dragging from it selects a range in Haze subtle.
- **Context menus**: app-drawn (egui), raised paper, r 10, `POP_SHADOW`, 5px inset; 26px items in 13px Geist with the toolbar's shortcut right-aligned in tertiary; the row under the pointer is graphite-inverse; a 12px check slot keeps labels aligned; submenus open to the right, 6px overlapped; an optional tertiary header names what was clicked ("Lines 42–44 · invoicer.go"). The menu belongs to what is under the pointer, or to the selection when the pointer is inside it. Every item repeats an action that exists elsewhere with the same key.
- **Empty states**: centred `empty_state` in the pane that is empty, never a picture. The title says what is empty in plain words ("No drafts on this Mac", "Nothing is running"); the body says why, what will fill it, or what was searched; one next action, usually secondary. When a list is empty the reading pane says "Nothing selected" instead of holding stale content. Completion is an empty state too: "All 4 files viewed" with a way to submit.
- Every component is drawn on the Figma page "Design System" → `DS · Components`.

## Do's and Don'ts

- Do show the key on every action. Don't add an action without one.
- Do label AI output "AI draft · not posted" until the reviewer keeps it.
- Do use authored, out-of-focus fields as imagery. Don't use sharp stock photos or gradients without grain.
- Don't put white text on the light zones of a field; use the Ember-night recipe or darker areas.
- Don't use Geist Mono as decoration.
- Don't recolour a third-party mark into Ember or Haze, and don't let one stand in for the PR Craft mark.
- Don't draw icons as text glyphs (✓ ! i ↕). Use SF Symbols rendered by `assets/sf/render.swift` (`COLOR=RRGGBB`). Typographic `›`, `→` and diff `+`/`−` stay text.
- Dark tertiary text is Night/350 `#A09EAA`, which keeps 4.5:1 up to `bg/raised` and haze-subtle. Text on an Ember or rose badge is white.
- Text is 11px or larger, avatar initials included: two letters from 22px up, one letter below. Native macOS chrome uses the Increase Contrast colours so white text passes: badge red `#D70015`, menu highlight `#0060C7`, secondary line white at 85%, a brand mark on the highlight in white.
