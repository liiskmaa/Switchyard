# Switchyard for dummies

A plain-language guide to using Switchyard: what it is, how to set it up, every setting you can
change, and how to do each kind of work with it. No Git or AI expertise needed. Where a technical
detail matters, it is explained.

> **Read this first: Switchyard is not a sandbox.** It decides what to *answer* when an AI agent
> asks to do something. When you approve a shell command, that command runs as you, with your
> permissions, and can touch any file on your Mac. Read every request before you approve it.

---

## Contents

1. [What Switchyard is, in one minute](#1-what-switchyard-is-in-one-minute)
2. [Words you will see](#2-words-you-will-see)
3. [Installing and starting it](#3-installing-and-starting-it)
4. [A tour of the app](#4-a-tour-of-the-app)
5. [Your first project, step by step](#5-your-first-project-step-by-step)
6. [Use cases: how to do each kind of work](#6-use-cases-how-to-do-each-kind-of-work)
7. [Answering agents: approvals](#7-answering-agents-approvals)
8. [Policies: deciding what may run without asking](#8-policies-deciding-what-may-run-without-asking)
9. [Delivering work into your branch](#9-delivering-work-into-your-branch)
10. [Every configuration option](#10-every-configuration-option)
11. [Cleaning up](#11-cleaning-up)
12. [When things go wrong](#12-when-things-go-wrong)
13. [What Switchyard will never do](#13-what-switchyard-will-never-do)
14. [Limits you should know about](#14-limits-you-should-know-about)
15. [FAQ](#15-faq)

---

## 1. What Switchyard is, in one minute

You have a code project and two AI coding assistants installed on your Mac: **Claude Code** and/or
**Codex**. Switchyard lets you:

- give them work ("add a dark mode toggle") and watch them do it;
- have one AI investigate, another implement and a third review, automatically;
- or let a "lead" AI build a small team, hand out subtasks and check every result;
- answer every risky thing they want to do (run a command, change a file) with **Approve** or **Deny**;
- see exactly what changed, run your tests, and only then merge the result into your branch, with
  your explicit approval.

Each AI works in its **own private copy** of your project (a separate folder on its own branch), so
it never touches the files you are working on, and several AIs can work at once without stepping
on each other.

Switchyard has two parts:

- **Switchyard Core**: a background program that runs the AIs, keeps a permanent record of
  everything, and does the Git work. It keeps running if you close the window.
- **The desktop app**: the window you click in. It only talks to Core.

Switchyard does not call any AI service itself and stores no passwords. It drives the `claude` and
`codex` programs you already installed and logged into.

---

## 2. Words you will see

| Word | Plain meaning |
|---|---|
| **Project** | A Git repository (code folder) on your Mac that you registered with Switchyard. Agents never work in it directly. |
| **Workspace** | A private copy of a project for agents to work in. Technically a Git *worktree*: its own folder and its own branch named `switchyard/<id>`. Stored in Switchyard's data folder, not inside your project. |
| **Session** | One running conversation with one AI (Claude Code or Codex) inside one workspace. At most one live session per workspace. |
| **Provider** | Which AI program: *Claude Code* or *Codex*. |
| **Model** | Which model that program uses (for example `opus` or `sonnet` for Claude Code). The list comes from the installed program itself. |
| **Task** | A durable piece of work: title, description, acceptance criteria ("done when…"). It outlives sessions. |
| **Assignment** | One job on a task given to one AI: *investigate*, *implement* or *review*. |
| **Report** | The structured summary an AI writes at the end of its work. It is only a *claim*; it never approves or completes anything by itself. |
| **Finding** | A problem a reviewer or investigator reported (with severity, file, suggestion). |
| **Proposal** | A subtask an AI suggests. You (or, in a team, Core) decide whether it becomes a real task. |
| **Workflow** | Switchyard running the steps for you. *Fixed sequence* (`engineering@1`): investigate → implement → review → correct. *Team* (`organization@1`): a lead supervisor plans and delegates. |
| **Approval** | A pending question: "the agent wants to do X; approve or deny?" |
| **Policy / rule** | Your standing instructions: what may run automatically, what must ask, what is blocked. |
| **Integration / delivery** | Taking the finished work from the workspace branch and merging it into your real branch (e.g. `main`). |
| **Validation commands** | Your checks (tests, linters, builds) that must pass before delivery. |
| **Target branch** | The branch work is delivered into. By default, the branch currently checked out in your project folder. |

---

## 3. Installing and starting it

### What you need

- A Mac (macOS 11 or later).
- Git and the Xcode Command Line Tools (`xcode-select --install`).
- Node 22.12 or newer (22 LTS or 24; with nvm, run `nvm use` in the repo), and npm 11 or newer (`npm i -g npm@11`).
- Rust (install from [rustup.rs](https://rustup.rs); the exact version installs itself).
- At least one AI program, installed and logged in:
  - Claude Code: `claude auth login`
  - Codex: `codex login`

### Run it

```sh
cd switchyard
npm install
npm run dev        # builds Core, opens the app; Core starts automatically
```

Or build a normal Mac app once with `npm run build`; it lands at
`target/release/bundle/macos/Switchyard.app` (not code-signed, so macOS may ask you to allow it).

### Try it without touching your real data

Switchyard keeps its database and workspaces in `~/Library/Application Support/Switchyard`. To
experiment in a throwaway place:

```sh
SWITCHYARD_DATA_DIR=/tmp/sy-try npm run dev
```

If that path is long, also set `SWITCHYARD_SOCKET=/tmp/sy-try.sock` (socket paths may be at most
103 bytes).

### Check the AI programs were found

Open **Agents** in the sidebar. It shows which programs were detected, their versions, the version
Switchyard was verified against, and what each one supports. Verified versions: Claude Code 2.1.322
(newer ones usually work), Codex 0.166.0 (other Codex versions are refused unless you opt in, see
[§10](#10-every-configuration-option)).

---

## 4. A tour of the app

| Sidebar | What it is for |
|---|---|
| **Home** | "Needs you": everything waiting for your decision, across all projects. Start here. |
| **Projects** | Register repositories; per-project settings (workflow roles, agent teams); workspaces; Git status. |
| **Tasks** | Create tasks, start workflows or teams, follow progress, answer questions, deliver. |
| **Integrations** | All deliveries (merges) and the per-project integration settings: review requirement and validation commands. |
| **Agents** | Detected AI programs and every session; open one to see its live output, stop or resume it. |
| **Activity** | The permanent journal of everything that happened. |
| **Approvals** | Every pending "may I?" request from agents. They also appear inline on tasks and sessions. |
| **Policies** | Rules for what runs automatically, what asks and what is blocked; and a log of every decision. |
| **Settings** | Read-only view of Core's connection, version and file paths; **Reconnect** button. |

The connection indicator shows whether the app is talking to Core. Closing the window does **not**
stop agents; Core keeps going and the app catches up when you reopen it.

---

## 5. Your first project, step by step

Start with a project you don't mind experimenting on (a copy, or a fresh branch).

1. **Projects → Add repository**, pick the folder of a Git repository. It must have at least one
   commit.
2. Still on the project page, look at **Workflow roles**. These say which AI does which step. The
   defaults are fine; press **Save defaults** to keep them, or change and **Save roles**.
3. **Integrations → Integration settings** (choose the project): add at least one validation
   command, e.g. name `Tests`, program and arguments `npm` / `test` on separate lines. Optional but
   strongly recommended: without one, approval will say "nothing was validated".
4. **Policies**: optionally add **Allow automatically** for the exact command `npm test` so the
   validation and the agents can run it without asking you every time.
5. **Tasks → New task**:
   - *Title*: "Add a --version flag to the CLI".
   - *Description*: what and why, in plain words.
   - *Acceptance criteria*: one per line, e.g. `prints the version from package.json`.
   - Leave *Publish now* and *Run the engineering workflow* ticked.
   - Press **Create and start**.
6. Watch the task page. Whenever something needs you, it shows at the top (and on **Home**):
   approve or deny tool requests, answer questions.
7. When the review approves the work, the task says **Ready for delivery** and walks you through:
   **Commit reviewed changes** → validation runs → **Approve these commits…** → it merges.
8. Done. Your target branch now has a merge commit with the work. Nothing was pushed anywhere.

---

## 6. Use cases: how to do each kind of work

### A. "Just let me talk to an AI in a safe copy" (single session)

1. **Projects** → your project → **New workspace** (name it; it branches from your current commit
   unless you choose a base).
2. Press **Start Claude Code here** or **Start Codex here** in that workspace, optionally with a first message.
3. Follow it under **Agents** → the session. Send more messages, **Interrupt** a turn, **Stop** the
   session, or **Resume** it later (it keeps its conversation).
4. The work stays in the workspace. To bring it into your branch, see [§9](#9-delivering-work-into-your-branch).

Good for: exploring, quick fixes, asking questions about the code.

### B. "Do this task for me, end to end" (fixed sequence workflow)

New task with *Run the engineering workflow* ticked, or on an existing ready task **Start workflow**
→ **Fixed sequence**. Switchyard then:

1. **Investigate**: one AI reads the code and reports findings.
2. **Implement**: an AI makes the change.
3. **Review**: an AI reviews it **read-only** (it cannot change files; its commands always ask).
4. If the review requests changes: **Correct** (up to 2 rounds automatically), then review again.
5. Approved review → **Delivery** (see [§9](#9-delivering-work-into-your-branch)).

It stops and waits for you ("Needs you") when:

| Situation | What you can do |
|---|---|
| Review still wants changes after 2 corrections, or rejects the work | **Resume** (one more correction), or take over |
| Review is *blocked* (e.g. files changed during review) | **Resume** reruns the review |
| An agent reports a blocker | Fix the cause, then **Resume** |
| An agent asks a question | **Send answer**; it continues |
| An agent twice gave no usable report | **Resume** (sends another reminder) |
| A session failed twice | **Resume** retries the step |
| Core was restarted mid-step | **Resume** (agents are never restarted on their own) |

Other controls: **Pause**, **Cancel workflow**, and *Advanced* on the task to assign steps by hand.

**Choosing who does what:** the project's **Workflow roles** set provider and model for
Investigation, Implementation, Review and Correction (Correction can *Inherit from Implementation*).
Override them for one task with **Override roles and models for this task** (new task) or
**Override roles for this task** (on start). The roles are frozen when the workflow starts.
Tip: use a *different provider* for Review than for Implementation; Switchyard warns you when they
are the same, because the review is less independent.

### C. "This is big; let an AI lead a team" (agent organization)

On a ready task: **Start workflow** → **Let a supervisor build the team**, pick the **Lead
supervisor** (Claude Code, Codex, or *Let Switchyard choose*) → **Start**.

What happens:

1. The lead plans: which agents it needs (specialists and possibly sub-supervisors), which subtasks,
   who owns which, scopes (which files each may touch), acceptance criteria and dependencies.
2. **Core checks the plan** against your limits and rules. A plan that breaks a limit changes
   nothing; the lead is told why.
3. Specialists work one at a time, each with ordinary approvals and policies.
4. If a specialist starts changing files **outside its scope**, Core interrupts it.
5. After each piece of work, its supervisor gets a checkpoint with evidence **from Git** (which files
   actually changed), not just the agent's word. It accepts, corrects, reassigns, cancels, adds
   agents or tasks, or **escalates** to you.
6. When everything is accepted, the lead reports the objective done → the usual **Delivery**.

On the task page you see **Who works on it**, the **Work plan**, **Decisions and supervision**, and
**Limits**. You can **Queue for next turn** an instruction to any agent ("Instructions reach an
agent at its next turn", never mid-turn), answer escalations, **Pause**, or **Stop the team**.

Configure teams per project under **Projects → Agent teams** (limits and your skill estimates,
see [§10](#10-every-configuration-option)).

> Teams are new and so far proven only with scripted test agents. Real models may write poor plans.
> Start with small objectives and keep an eye on it.

### D. "I want to drive every step myself" (manual tasks)

Create a task with *Run the engineering workflow* unticked. On the task page use **Assign and
start**: choose the role (*investigate*, *implement*, *review*), the provider and the workspace.
After each report the task waits for you; you can then:

- **Accept and complete** the task;
- assign a review, or another implementation (open findings are passed along automatically);
- **Send note** to a running agent;
- **Mark as failed**, **Retry the task**, or **Cancel the task**.

Proposals (subtasks an AI suggests) appear on the task: **Approve as draft** or **Reject**. Drafts
become real work only when you **Publish** them.

### E. "Only investigate" / "only review"

- *Investigate*: create a task of kind **investigation**, assign an investigate role. You get
  findings and maybe proposals; nothing is changed.
- *Review existing work*: assign **review** in the workspace that holds the work. The reviewer is
  read-only, and the workspace is scanned before and after; if anything changed, the verdict is
  forced to *blocked*.

### F. Tasks that depend on each other

In **New task**, pick **Depends on**; the task waits until those are completed. On a task, **Wait
for another task…** adds a dependency later. Circular dependencies are refused. A task with open
dependencies cannot start a workflow until they are done.

### G. Several things at once

Every workspace is independent. Create several workspaces (or several tasks; each workflow makes its
own) and they run in parallel. Within one task or one team, steps run one at a time.

### H. Merge someone else's (or your own) workspace branch

**Integrations** → choose the project → prepare an integration from a workspace (or from a task:
**Prepare integration**). Then follow [§9](#9-delivering-work-into-your-branch).

---

## 7. Answering agents: approvals

When an agent wants to do something a policy says *ask* about, you get an approval card on **Home**,
**Approvals**, the task, and the session. Each card shows:

- **What was asked**: the tool and its input (the command, the file).
- **Reaches**: which files or folders it touches, and whether they are inside the workspace.
- **Why it asks**: which rule or default caused the question.

Press **Approve** or **Deny** (optionally **Add a reason**). Things to know:

- If nobody answers within 15 minutes (configurable), the request is **denied** automatically. The
  card shows the countdown. Requests are never approved by timeout.
- Approving one request **never** creates a rule. Want it automatic next time? Add a policy, or
  turn on [auto mode](#auto-mode-stop-the-approve-clicking) for the project.
- If you change a rule so the pending request is now blocked, it is denied right away.
- Approved shell commands run with *your* permissions; an approved Codex command is **not** confined
  to the workspace.

---

## 8. Policies: deciding what may run without asking

**Policies → Add rule.** Fields:

| Field | Options |
|---|---|
| **Applies to** | *Every workspace of this project*, or *Only workspace "…"* (fixed after creation) |
| **Operation** | Read files · Create or overwrite files · Edit files · Delete files · List directories · Search files · Run shell commands · Network access · External tools (MCP) (fixed after creation) |
| **Decision** | **Allow automatically** · **Ask every time** · **Block** |
| **Only under** | optional folder or file, relative to the workspace (e.g. `docs`, `src/generated`) |
| **Exact command** | for shell: the exact command line (required for *Allow*), e.g. `cargo test` |
| **Description** | required: why the rule exists |

**Defaults (no rule):**

| Operation | Default |
|---|---|
| Read, search, list inside the workspace | Allow |
| Create, edit, delete files | Ask |
| Shell commands | Ask |
| Network, external (MCP) tools | Block |
| Anything unrecognized | Block (cannot be changed) |

**How rules combine:** the **strictest matching rule wins** (Block > Ask > Allow), whether it is a
project or a workspace rule. You cannot loosen a project rule with a workspace rule.

**Safety caps** turn an *Allow* back into *Ask* when: the target is outside the workspace (or can't
be resolved), something writes into `.git`, the request has unexpected fields, a command would run
in the background, or a shell command is not an *exact* match of an allow rule (`cargo test --all`
does not match `cargo test`).

**Not possible:** allowing shell without an exact command; allowing network/MCP tools; any rule for
unrecognized operations; duplicate rules.

**Review sessions** cannot change files no matter what your rules say.

Rules can be edited (decision, path, command, description), disabled, or archived. Every change is
versioned, and the **Policies** page lists every past decision with the rule revision that made it.

### Auto mode: stop the approve-clicking

Tired of approving every command? Turn on **auto mode** for the project: **Projects → your project →
Auto mode → Turn on auto mode…**, then confirm (the same switch is on the **Policies** page).

With auto mode on:

| Request | Answer |
|---|---|
| Create, edit, delete files inside the workspace | Runs without asking |
| Any shell command (pipes, `&&`, `npm install`, `rm`, …) | Runs without asking |
| Validation commands at delivery | Run without asking for authorization |
| Files outside the workspace, writes into `.git` | Still asks |
| Background (long-running) commands, requests with unknown fields | Still asks |
| Your own **Ask every time** and **Block** rules | Still win |
| Review sessions | Unchanged: cannot change files, commands ask |
| Network and MCP tools | Still blocked |

Things to know:

- **This is the risky switch.** A shell command can't be confined to the workspace: with auto mode
  on, an agent can run anything your user account can, anywhere on your Mac, without you seeing it
  first. Use it on projects (and machines) where that is acceptable.
- Add **Block** rules for commands you never want run, e.g. `git push` or `rm -rf /`. They still
  apply.
- Tasks and sessions of the project show an **Auto mode** badge while it is on.
- Requests already waiting when you turn it on still need your answer.
- Every automatic decision is still recorded (reason "Auto mode") under **Policies**.
- Turn it off the same way; from then on everything asks again.

**Useful starter rules**

- *Allow automatically* · Run shell commands · `npm test` (or `cargo test`, `pytest`).
- *Allow automatically* · Edit files · only under `docs`, for documentation-only work.
- *Block* · Delete files (if you never want agents deleting files).
- *Ask every time* · Read files · only under `secrets` (make reads of sensitive folders ask).

---

## 9. Delivering work into your branch

Delivery (integration) takes the work from a workspace branch into your **target branch** (by
default whatever branch your project folder has checked out). In a workflow it starts by itself
after an approved review; otherwise use **Prepare integration**.

The steps, and what you press:

| Phase | What it means | You |
|---|---|---|
| Preparing | Switchyard reads the facts from Git | — |
| Needs commit | Agents leave work uncommitted | **Commit reviewed changes** (shows the files first) |
| Out of date | Your target moved on | **Update workspace from <branch>** (merges the target into the workspace) |
| Conflicts | The update conflicts | Fix in the workspace (yourself or by assigning an agent), then *Complete resolution*, or *Abort* |
| Validating | Your validation commands run for the exact commit | — (*Cancel validation* if needed) |
| Needs authorization | Some commands are *ask* under your policies | **Authorize and validate** |
| Validation failed | A check failed | **Run validation again** or **Send back for correction** |
| Blocked | Something is in the way (see list below) | Fix it, then **Check again** |
| Awaiting approval | Shows exact commits, files, diff, review and validation results, and **What approving does** | **Approve these commits…** then confirm, or **Reject integration** |
| Integrating → Delivered | Merge commit created, target branch moved | — the task completes in your name |

**Common blockers and fixes**

| Blocker | Fix |
|---|---|
| An agent is working | Wait, or stop the session |
| Uncommitted changes | **Commit reviewed changes** |
| Target checkout has local changes | Commit or stash your own work in the project folder |
| Git has no identity | `git config --global user.name "…"` and `user.email "…"` |
| Review required | Run a review of exactly this content (or turn the requirement off) |
| Validation required / failed | Run validation until it passes |
| No changes | There is nothing to deliver |

**Guarantees:** only the exact two commits you approved are merged; if anything moved in between,
the approval is refused and re-prepared. Your uncommitted work and ignored files (like `.env`) are
never overwritten. Nothing is ever pushed.

---

## 10. Every configuration option

### 10.1 Environment variables (set before starting the app or Core)

| Variable | Default | What it does |
|---|---|---|
| `SWITCHYARD_DATA_DIR` | `~/Library/Application Support/Switchyard` | Where the database, logs, socket and all workspaces live |
| `SWITCHYARD_SOCKET` | `<data dir>/core.sock` | The connection between app and Core (max 103 bytes) |
| `SWITCHYARD_CORE_BIN` | `switchyard-core` next to the app | Which Core program the app launches |
| `SWITCHYARD_AUTO_LAUNCH_CORE` | `1` | `0`: the app does not start Core; you run it yourself |
| `SWITCHYARD_GIT` | `git` on your `PATH` | Which Git to use |
| `SWITCHYARD_CLAUDE_BIN` | `claude` on `PATH`, then usual install places | Which Claude Code program to use |
| `SWITCHYARD_CODEX_BIN` | `codex` on `PATH`, then usual install places | Which Codex program to use |
| `SWITCHYARD_ALLOW_UNTESTED_CODEX` | unset | `1` allows a Codex version other than 0.166.0 (logged as a warning) |
| `SWITCHYARD_APPROVAL_TIMEOUT_SECS` | `900` (15 min) | Unanswered approvals are denied after this long |
| `SWITCHYARD_LOG` | `info` | Log detail (`debug`, `trace`, or a `tracing` filter) |

Example: `SWITCHYARD_APPROVAL_TIMEOUT_SECS=3600 SWITCHYARD_LOG=debug npm run dev`

### 10.2 Running Core by itself

```sh
switchyard-core [--data-dir PATH] [--socket PATH] [--log-format text|json] [--version]
# during development:
cargo run -p switchyard-core -- --log-format text
```

Start the app with `SWITCHYARD_AUTO_LAUNCH_CORE=0` and it connects to the Core you started.

### 10.3 Settings page (read-only)

Shows connection state, Core version, protocol version, process ID, start time, and the paths in
use (data directory, socket, database, Core program, Core log). **Reconnect** forces a new
connection. To change paths, use the environment variables above.

### 10.4 Per project: Workflow roles (Projects → *Workflow roles*)

For each role, a **Provider** (Claude Code / Codex) and a **Model**:

| Role | Used for |
|---|---|
| Investigation | the first look at the task |
| Implementation | making the change |
| Review | read-only checking |
| Correction | fixing review findings (default: *Inherit from Implementation*) |

Model choices: **CLI default**, the models the installed program reports, or **Other full model
name…** (for Claude, a full `claude-…` name; checked only when the session starts). Tasks can
override any role.

### 10.5 Per project: Auto mode

**Projects → Auto mode** (or **Policies → Auto mode**): off by default. On = file changes and shell
commands inside the workspace run without asking; see
[Auto mode](#auto-mode-stop-the-approve-clicking) for exactly what still asks.

### 10.6 Per project: Agent teams (Projects → *Agent teams*)

**Limits** (a plan that would exceed them is refused):

| Setting | Default | Meaning |
|---|---|---|
| Agents per task | 6 | how many agents a team may have |
| Levels of supervision | 3 | how deep the supervisor tree may go |
| Subtasks per task | 12 | including the objective itself |
| Corrections per subtask | 2 | correction rounds before it escalates |
| Supervision turns | 30 | total supervisor turns |
| Minutes before it pauses | 240 | wall-clock budget, then it waits for you |

Also fixed for now: 1 session at a time per team, 1 automatic retry of a failed turn; cost and
token limits are off.

**Your estimates**: for each provider and skill (reasoning, coding, research, review,
documentation, testing, long context) pick *Not set*, *Basic*, *Good*, *Strong* or *Never use for
this*. Switchyard ships **no opinions** about which AI is good at what; it uses only verified
capabilities and your estimates, prefers a different provider for review than for implementation,
then the cheaper one. Without estimates it says so in its choice. Hover an agent's name on the task
to see why it was chosen.

### 10.7 Per project: Integration settings (Integrations → *Integration settings*)

- **Require an approved review of exactly the integrated content**: on = delivery is blocked until a
  review approved exactly this content.
- **Validation commands**: *Name*, *Timeout (seconds)*, *Program and arguments, one per line*
  (e.g. `cargo` / `test`). They run directly, never through a shell, so pipes, `&&` and `$VARS` do
  not work; write a script and run that if you need them. Whether each runs automatically is decided
  by your shell policies: *allow* runs, *ask* waits for **Authorize and validate**, *block* never
  runs.

### 10.8 Per task (New task form)

| Field | Notes |
|---|---|
| Title | required, up to 200 characters |
| Description | what and why; the agents read it |
| Kind | implementation · investigation · general |
| Priority | low · normal · high · urgent |
| Acceptance criteria | one per line; reviewers and supervisors check against them |
| Depends on | tasks that must complete first |
| Publish now | off = stays a draft until you **Publish** |
| Run the engineering workflow | starts the fixed sequence right away (needs: published, no dependencies) |
| Override roles and models | per-task provider/model per role |

### 10.9 Per workspace

Name and base revision (what it branches from) when you create it. Rules can be scoped to one
workspace (see [§8](#8-policies-deciding-what-may-run-without-asking)).

---

## 11. Cleaning up

Workspaces stay until **you** remove them. The lifecycle:

1. **Mark as done** (optional): marks the workspace finished; a new session reopens it.
2. **Archive…**: hides it; files stay on disk. **Restore** brings it back.
3. **Clean up…** (only for archived workspaces): shows exactly what would be deleted, refuses if
   there is uncommitted or untracked work, and asks you to confirm the exact state you saw. It
   deletes the folder but **keeps the branch**, so the commits are never lost.

To remove a project from Switchyard (**Remove registration**), first archive and clean up its
workspaces. Your repository is never touched.

Disk space: every workspace is a full checkout of your project, plus whatever builds the agents run
in it (`node_modules`, `target/`…). Clean up finished ones.

---

## 12. When things go wrong

| Symptom | What to do |
|---|---|
| App says it cannot connect to Core | **Settings → Reconnect**. Check the Core log path shown there. Make sure only one Core uses the same data dir. |
| "Incompatible Core" | The app and Core versions don't match; rebuild both (`npm run dev` rebuilds Core). |
| An agent program isn't listed under **Agents** | Install and log in; or point `SWITCHYARD_CLAUDE_BIN` / `SWITCHYARD_CODEX_BIN` at it. |
| Codex refused as untested | Use Codex 0.166.0, or set `SWITCHYARD_ALLOW_UNTESTED_CODEX=1` at your own risk. |
| Workspace shows **failed** | Its folder or branch went missing or changed. Read the reason; fix it, then **Re-check**. |
| Task/workflow is waiting after a restart | Expected: agents are never restarted automatically. Press **Resume**. |
| Agent "gave no usable report" | It didn't end with the report block (or wrote too much text). **Send note** asking it to finish with its report, or **Resume**. |
| Validation command "not found" | A Core started from the app may have a minimal `PATH`. Use a full path (`/opt/homebrew/bin/npm`), or start the app from a terminal. |
| Delivery stuck at *integrating* after a crash | Press **Check again**; Core decides from Git. If it says the checkout may be partially updated, inspect it with `git status`. |
| A request was denied but you didn't deny it | It timed out (15 min by default), a rule blocks it, or the session ended. The card and the policy log say which. |
| Disk filling up | Archive and clean up old workspaces. |

Everything is in the journal (**Activity**) and the Core log, so you can always see what happened.

---

## 13. What Switchyard will never do

- Push, force-push, rebase, reset, or delete branches.
- Merge anything without you approving the exact commits.
- Overwrite your uncommitted work or ignored files (like `.env`).
- Approve a request because of a timeout, a report, or an AI's say-so.
- Create a policy rule because you approved something.
- Delete a workspace on its own, or restart agents on its own after a crash.
- Store your AI logins or call AI services itself.
- Substitute a model you didn't choose.

---

## 14. Limits you should know about

- **Not a sandbox.** Approved commands run as you, anywhere on your Mac. Codex-approved commands
  are not limited to the workspace. With auto mode on, that applies to every command an agent runs.
- **One step at a time** per task and per team; parallelism comes from separate tasks/workspaces.
- **Corrections reach the next turn**: you can't redirect an agent in the middle of a turn.
- **Agent reports can be wrong or missing.** Long final messages (over about 8 KB) lose their report.
- **Teams are the newest part** and proven only with scripted test agents so far.
- **No timeouts on a turn that never ends**: it waits for you.
- **No provider fallback**: if a chosen AI is unavailable, the step waits.
- **Repository hooks don't run** for Switchyard's commits and merges; put those checks in validation
  commands.
- **Local only**: no pushing, pull requests or deployment.
- macOS only for the app.

---

## 15. FAQ

**Does closing the app stop my agents?** No. Core keeps running. Reopen the app to catch up.

**Does Switchyard cost money?** Switchyard itself no, but the AI programs use your Claude / ChatGPT
accounts and their usage limits or billing. The session view shows cost and tokens where the
program reports them.

**Can I keep working in my project folder while agents work?** Yes. Agents work in separate
folders. Just keep your target branch's checkout clean when you approve a delivery.

**Where are the workspaces on disk?** `<data dir>/workspaces/<project>/<workspace id>`. The
branch is `switchyard/<workspace id>` in your repository.

**I approved something by mistake. Can I undo it?** The action already ran. Switchyard doesn't undo
it. Inspect the workspace (or your system) and fix it; the journal shows exactly what was approved.

**Can an AI approve its own work or merge it?** No. Reports are claims. Only you complete tasks
(or, in a team, its supervisor accepts subtasks), and only you approve a delivery.

**I'm clicking Approve all day. Can it just do things?** Yes: turn on
[auto mode](#auto-mode-stop-the-approve-clicking) for the project. Read what it means first.

**Which AI should review?** A different one from the implementer, if you have both. Switchyard
warns when they're the same.

**How do I try a change without risk?** Use a separate data dir (`SWITCHYARD_DATA_DIR=/tmp/sy-try`)
and a throwaway copy of the repository.

---

More detail for the curious: [architecture overview](architecture/overview.md),
[workflows](architecture/workflows.md), [teams](architecture/organizations.md),
[policies](architecture/policies.md), [delivery](architecture/integrations.md),
[workspaces](architecture/workspaces.md), [tasks](architecture/tasks.md).
