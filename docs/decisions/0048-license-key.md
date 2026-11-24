# ADR 0048: A 30-day one-project trial, then a signed license key

- Status: accepted (2026-11-24); not implemented yet

## Context

Switchyard is public under the Functional Source License 1.1 with the MIT future license
(FSL-1.1-MIT, `LICENSE.md`); versions published before 2026-11-24 stay MIT. FSL forbids only a
Competing Use: offering Switchyard, or what it does, to others as a commercial product or service.
It explicitly permits internal use, including modification. Each version becomes MIT two years
after its release.

The author's model (decided 2026-11-24): Switchyard can be tried for free in **one project for 30
days**, with everything it does. After that, using it needs a license key, bought with a **one-time
payment of €29.99 that includes a year of updates**. Keys are sent by email for now; a central
portal may issue them later.

What the license does and does not do, stated plainly so no part of the design pretends otherwise:

- FSL stops competitors from selling Switchyard. It does **not** oblige anyone to buy a key: a
  person or company may build from source and, for internal use, remove the check. The key is an
  honest-customer mechanism, not copy protection. Its value is the signed, notarized, auto-updating
  app, the author's support, and making paying the easy, normal path.
- Source is public, so obfuscation, hidden checks or tamper detection would cost effort and add
  bugs while stopping nobody. They are out of scope.

Constraints from the architecture: Core owns everything long-lived, the webview only asks; Core runs
offline and sends nothing anywhere except to the agent CLIs the user starts; agent work must never
be broken mid-way, nor trapped, by a licensing decision.

## Decision

### The trial: one project, 30 days

1. Without a key, Switchyard runs as a **trial**. Registering projects, browsing, settings and
   policies are always free. The trial starts at the **first new work** (rule 9) in any project:
   that moment starts the 30 days, and that project becomes **the trial project**. Core records
   both in the `license` table, with `license.changed` (`{edition: "trial", trial_project_id,
   trial_ends_at}`).
2. During the trial, the trial project has every feature: sessions with every provider, workspaces,
   tasks, workflows, teams, the goal keeper, the supervisor, integration and delivery. Starting new
   work in **any other project** is refused (`license_required`, "The trial covers one project:
   <name>"). The trial project cannot be changed; removing it from Switchyard does not free the
   trial.
3. After the 30 days (by the clock), starting new work is refused everywhere until a key is
   entered: "Your trial ended". Rule 10 says what keeps working.
4. One trial per data directory. Deleting the data directory or changing the clock restarts or
   stretches it; accepted (see Context).

### The key: a one-time payment with a year of updates

5. A license key is one line of text: `SWY1.<payload>.<signature>`, both parts base64url without
   padding. `payload` is UTF-8 JSON; `signature` is an Ed25519 signature over `SWY1.` and the
   payload part (the version prefix is signed too).

   ```json
   {"v": 1, "id": "01a2…", "licensee": "Name or company", "email": "who@example.com",
    "edition": "full", "issued_at": "2026-11-24", "updates_until": "2027-11-24",
    "expires_at": null, "features": []}
   ```

   A key unlocks everything, in every project. `features` is empty today and reserved for later
   add-ons, so they need no new format. Unknown fields are ignored (keys stay valid in newer
   versions); an unknown `v` is refused with "this key needs a newer Switchyard".
6. A key costs **€29.99**, one time. A sold key has `updates_until` = purchase date + 1 year: it unlocks every Switchyard version
   **released** on or before that date, **forever**. Core compares it with its own build date
   (compiled in), never with the clock, so the versions paid for keep working without end and
   changing the clock does nothing. A version released after that date tells the licensee their
   year of updates ended and runs like an ended trial (rule 10) until they buy another year (a
   renewal key with a later `updates_until`) or go back to a version their key covers. There is no
   subscription. `expires_at` exists only for keys the author grants for a limited time; a key the
   author grants can also have no dates.
7. The public key is compiled into Core. The private key never enters the repository, CI or any
   machine that runs agents: it lives in the author's password manager and signs keys with a small
   issuing tool kept outside this repository. Keys are emailed by the author for now. A central
   portal later (payment, download, renewal, lost keys) issues the **same** keys from a signing
   server; Core needs no change for it and never contacts it.
8. A key is per person (or per company seat), for any number of that person's machines; there is
   no machine count or activation.

### Verification, storage and where Core checks

9. **New work** is exactly: starting an agent session (`agents.start`), starting a task assignment,
   `workflows.start` (engineering or team) and `tasks.start_again`, and the task supervisor's first
   message on a task. Core checks the license there, in the handler, before anything is written,
   against the work's project. A missing entitlement is the protocol error `license_required`
   (`data: {reason: "trial_other_project" | "trial_ended" | "updates_ended", project_id?}`), which
   the UI shows with the reason and the way to enter or buy a key; controls stay visible, not
   hidden.
10. **Never trapped, never taken away.** Ending a trial, or a key that no longer covers this
    version, never stops, pauses or cancels running work, and never blocks finishing what exists:
    a running workflow or team runs to its end (its own steps, reviews, the goal keeper and
    supervision turns are not new work); interrupted work can be resumed; pending approvals can be
    answered; finished work can be validated, integrated and delivered; workspaces can be
    archived and cleaned up. Every record stays readable. Only rule 9 is refused.
11. A pure crate `switchyard-license` parses and verifies a key and decides
    `check(license_state, key, build_date, now, project) -> Allowed | Refused(reason)`. No I/O;
    unit-tested with a test keypair. Core is the only caller.
12. Core verifies offline, at startup and when a key is entered. There is no activation server, no
    phone-home, no telemetry and no machine fingerprint. The key and the trial are stored in SQLite
    (a new migration: a single-row `license` table) and changes are journaled as `license.changed`
    (never the key text). The key text is never logged and never sent to the webview after it is
    entered.
13. Protocol: `license.get` (edition: trial / full; for a trial the project and days left; for a
    key the licensee, `updates_until` and whether it covers this version), `license.set {key}`
    (verifies first, stores only a valid key; `license_invalid` with the reason: signature, format,
    version, "for versions released before …"), `license.clear`. All are in `ALLOWED_METHODS` and
    `CoreMethods`. The UI shows the state where it matters: a quiet "Trial: 23 days left ·
    <project>" in the header, the License section in Settings (state, licensee, dates, a field to
    paste a key, how to buy one), and the refusal message on a refused start.
14. Tests and development: the test keypair's public key is accepted only in `cfg(test)` and debug
    builds (`debug_assertions`), so integration tests can unlock with keys they sign themselves;
    `tests/common` starts every test Core with such a key unless a test is about the trial.
    Release builds know only the real public key. (A user building from source can change that;
    see Context.)
15. Upgrading: an existing data directory with work in several projects gets no trial project
    retroactively; its trial starts at the first new work after the upgrade, in that work's
    project. The author issues himself a key.

## Consequences

- Trying is one click away and needs no email; paying is the normal path after 30 days: download
  the signed app, paste a key. Nothing is ever disabled under a user mid-task, nothing is trapped,
  and Switchyard keeps working fully offline.
- The trial is generous inside its one project (everything) and strict outside it, which is easy
  to explain and has a single boundary to check.
- A determined internal user can remove the check; FSL allows that. Accepted: the people who pay
  are the people who would pay anyway, and a hostile check would hurt them, not the others.
- Losing the private key means re-issuing every key with a new public key in a new release; leaking
  it means anyone can mint keys until a release changes the public key. It is backed up offline.
- Adding the error codes `license_required` and `license_invalid` and three methods is a compatible
  protocol addition; an older desktop shows the error's message.
- The integration tests need a test key by default, or every test that starts work would be
  refused once the trial logic exists.

## Alternatives considered

- **A permanent free tier with Pro features** (an earlier draft of this ADR): rejected by the
  author; the trial is the free part.
- **Online activation or a licensing server:** catches shared keys, but needs the network, sends
  data about the user, can lock people out when the server is down, and breaks Core's rule of
  sending nothing anywhere. Rejected; a future portal issues keys but Core never calls it.
- **A trial key by email:** knows who tries, but adds friction and a server or manual step before
  the first try. Rejected for the trial; keys are for paying users.
- **BSL or PolyForm instead of FSL** (licenses that require payment for some internal use): legal
  weight behind the key, but a less friendly license and still unenforceable without audits. The
  author chose FSL.
- **A subscription:** steady income, but Switchyard is a local tool that keeps working offline;
  the author chose a one-time payment with a year of updates.

## Decided by the author (2026-11-24)

- €29.99 **including VAT** for a key with a year of updates. VAT on digital sales to consumers is
  charged at the buyer's country rate (EU OSS), so the net amount differs per country; a merchant
  of record (for example Paddle or Lemon Squeezy) collects and remits it when sales move to the
  portal.
- A renewal year costs the same, €29.99.
- One key per person; no company editions or seats. A company buys a key per person.
- A trial is not extended inside Core; the author can grant a time-limited key by email instead.
