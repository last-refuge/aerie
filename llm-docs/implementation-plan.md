# Aether implementation plan

**Status:** Proposed implementation roadmap; no application code exists yet.
**Prepared:** October 19, 2026.
**Design authority:** [Aether brand guidelines](brand-guidelines.md).

## 1. Product outcome and assumptions

Build a premium, Linux-native Google Drive app that makes cloud files feel at home on the desktop. The experience must communicate speed, seamlessness, precision, and quiet sophistication. Use “Google Drive for Linux” as the descriptor, “Your files, everywhere.” as the primary tagline, and “Your cloud, at home on Linux.” as the brand promise.

The initial target is an individual using one Google account on Arch Linux or Omarchy under Wayland. The application must work independently of Omarchy and receive compatibility testing on GNOME and KDE. Treat the technology choices below as recommendations to validate in Phase 0. The on-device data boundary in Section 3 is an accepted requirement, not an optional technology choice.

Deliver a native browser and manual transfers first, followed by reliable selective, bidirectional synchronization of ordinary files. A browser-only preview is an intermediate milestone; the first stable release includes selective sync. No calendar deadline is committed: progress depends on passing the phase gates and resolving external authorization requirements.

## 2. Release scope

| Capability | First stable release | Later releases |
| --- | --- | --- |
| Accounts | One Google account; sign in, reconnect, disconnect | Multiple accounts and account switching |
| Browsing | My Drive, breadcrumbs, sorting, metadata, cached filename search | Shared drives, Shared with me, advanced search and rich previews |
| File actions | Create folder, rename, move, upload, download, open locally, open in Drive, move to trash | Sharing permissions, revision browser, advanced bulk actions |
| Synchronization | Explicitly selected My Drive folders mirrored into one local root; pause, resume, exclusions, conflict resolution | Multiple roots, advanced policies and bandwidth scheduling |
| Offline use | Browse cached metadata and edit downloaded, selected files; queue reconciliation until connected | On-demand filesystem mount or placeholder files |
| Google Docs, Sheets, Slides | Browser launch and explicit one-way exports where supported | Additional export workflows; native collaborative editing is outside the roadmap |
| Desktop integration | Launcher, notifications, optional background operation and tray, open/reveal in file manager | File-manager emblems and deeper shell extensions |
| Distribution | Arch package first; documented source build | Flatpak after filesystem and background-operation validation; other packages |

Do not imply that cached metadata means file contents are available offline. Do not expose controls for deferred features until they work. Exclude permanent remote deletion, automatic content merging, and telemetry from the initial release. A hosted backend that accesses Google Drive credentials or data is prohibited throughout this roadmap.

## 3. Recommended architecture

### Required on-device data boundary

All Aether processing of Google Drive access and data must run on the user's computer. The only remote services involved in authentication and Drive operations are Google's own authorization and Drive services, contacted directly from that computer. This does not mean files cease to exist in Google Drive: Google remains the remote source and sync destination.

```text
User's computer                                  Google services
  System browser + local OAuth callback  <----->  Google authorization
  Aether UI + local sync service          <----->  Google Drive API/content
  Credential store, SQLite, index, files

Aether hosting: public website, documentation and release artifacts only;
no access to Google credentials or Drive data.
```

- Perform authorization-code exchange, token refresh and revocation locally against Google. Never route authentication through an Aether-hosted callback, token broker or API proxy.
- Keep access/refresh tokens, OAuth codes and PKCE material local except for their required exchange with Google. Store long-lived credentials in the local system credential service; do not upload credential backups.
- Keep file contents, names, IDs, folder trees, permissions, account metadata, thumbnails, previews, search indexes, change cursors and transfer/session state local except for direct Google requests. Generate previews and search indexes on-device; do not use hosted indexing, conversion, AI or preview services.
- Run `aether-service` exclusively on the same computer as the UI. Use same-user local IPC; expose no remote control endpoint or hosted sync worker. A loopback OAuth listener is temporary and local, not an Aether cloud service.
- Keep diagnostics and crash artifacts local. Provide only a locally generated, redacted diagnostic export with no automatic upload. Never include tokens, file contents or Drive-identifying metadata in a support bundle intended for external sharing.
- Keep public website, documentation, release downloads and any future update checks independent of Google accounts. They must receive no Google credentials, Drive identifiers, filenames or content, including through URLs, query parameters or logs. Package downloads do not require an Aether account.
- Opening a file in Drive or a Google editor launches Google's URL directly in the user's browser. Local open/reveal operations use desktop integrations without an Aether relay.

These constraints apply to later releases as well as the first release. A feature requiring hosted access to Drive data requires an explicit change to this product requirement and a fresh Google verification/assessment review before implementation.

### Application layers

Use Rust for the application and sync engine, with GTK4 through gtk-rs and libadwaita for native controls and window structure. gtk-rs supplies Rust bindings for GTK4 and the underlying GNOME libraries. This recommendation favors native interaction and existing widget accessibility; Phase 0 must prove that Aether's styling can be implemented without fragile overrides. [gtk-rs documentation](https://gtk-rs.org/)

| Layer | Responsibility | Proposed approach |
| --- | --- | --- |
| Desktop UI | Screens, selection, commands, focus, accessibility and presentation state | GTK4/libadwaita; reusable branded components |
| Application core | Account, file and transfer models; commands and events | Rust types independent of GTK and HTTP |
| Drive adapter | OAuth, listing, metadata, changes, uploads and downloads | Drive API v3 over an asynchronous Rust HTTP client |
| Sync engine | Reconciliation, scheduling, retries, conflicts and recovery | Rust worker tasks; persistent operation queue |
| Persistence | Cached metadata, local mappings, sync baselines and migrations | SQLite with transactional writes |
| Linux integration | Credentials, portals, notifications, launcher and background lifecycle | Secret Service, desktop APIs and an optional user service |

Keep network requests, database work, hashing and filesystem scans off the GTK main thread. Deliver bounded, coalesced updates to the UI so a busy queue does not flood rendering. Choose and pin compatible stable dependency versions during the initial spike.

Start with one application process and background worker tasks. Before shipping sync, separate the engine into a per-user `aether-service` process with a documented local IPC interface. The service owns the database and queue; the UI sends commands and subscribes to events. Enforce single ownership, restrict IPC to the current user, version the protocol, and surface version mismatch clearly. Closing a window must not accidentally terminate opted-in background sync; an explicit “Quit Aether” action must describe whether it also stops sync.

### Proposed repository layout

```text
crates/
  aether-app/       # GTK application, views and components
  aether-core/      # Shared domain models and commands
  aether-drive/     # OAuth and Drive API adapter
  aether-sync/      # Reconciliation and transfer scheduling
  aether-store/     # SQLite schema and migrations
  aether-service/   # Background process and IPC, introduced before sync release
assets/
  branding/        # Approved logo variants and application icons
  icons/           # Lucide UI assets and symbolic tray icons
  styles/          # Theme tokens and GTK styles
packaging/         # Desktop entry, metadata, service and package definitions
tests/             # Integration fixtures and recovery scenarios
llm-docs/          # Brand, architecture and implementation documentation
```

Use XDG configuration, data, cache and state locations. Keep the user-selected sync root separate from disposable caches. Store credentials in the system credential service, never in SQLite or configuration files. If no credential store is available or it is locked, offer a clear unlock/setup path or a session-only login; do not silently fall back to plaintext storage.

## 4. Translate the brand into the product

### Design tokens and assets

Create one semantic token system consumed by all components. The following values come from the brand guidelines and are the dark-theme baseline.

| Token or treatment | Required value or behavior |
| --- | --- |
| Window / sidebar / panel | `#0B1220` / `#101B2D` / `#1E293B` |
| Subtle border | `#334155` at 50% opacity; increase contrast where needed for usability |
| Primary / secondary text | Snow `#F8FAFC` / Mist `#94A3B8` |
| Brand accents | Aether Blue `#38BDF8`, Nebula Purple `#8B5CF6`, Aurora Cyan `#06B6D4` |
| Signature gradient | Blue → indigo → violet → cyan; indigo stop and positions require design selection |
| Visual balance | Approximately 75% neutral surfaces, 15% type/structure, 10% accents |
| Font families | Inter for interface text; JetBrains Mono for paths and technical details |
| Type scale | Page 32px/650–700; section 22px/600; UI heading 16px/600; body 14px/400; metadata 12px/400; technical 12px/500 mono |
| Marketing hero | 64–80px/700, reserved for promotional surfaces |
| Geometry | Cards 12px, buttons 10px, dialogs 16px; 4px spacing base with 8/12/16/24/32px steps |
| Icons and motion | Lucide line icons, nominal 1.75px strokes; motion 120–220ms |

Implement semantic colors for selection, focus, disabled controls, success, warning and error. These status colors and the light palette are new design decisions, not values already defined by the brand document. Validate contrast before accepting them. Use text and icons as well as color to communicate state.

Provide System, Dark and Light appearance settings and an accent preference with system/default and user overrides. Prefer the dark brand appearance when no system preference exists. Keep the logo gradient consistent while allowing control accents to change. Libadwaita documents dark/light appearance, accents and high contrast; custom styles must preserve these behaviors. Validate against the pinned stable release rather than assuming every API in development documentation is available. [Libadwaita styles and appearance](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/styles-and-appearance.html)

Use the supplied brand image as a visual reference. Prepare production assets separately: standalone cloud symbol, horizontal logo, vertical lockup, monochrome mark and size-appropriate application/tray icons. Maintain clear space and avoid busy backdrops. Check asset and font licenses before distribution.

### Screen and interaction plan

| Surface | Implementation intent |
| --- | --- |
| Welcome and account setup | Cloud symbol, concise promise, one sign-in action, accessible progress and recoverable errors |
| Main browser | Compact sidebar, search, breadcrumbs, sortable virtualized file list and quiet account/sync status |
| Search | Keyboard shortcut, immediate cached filename results, clear indexing/offline state and cancellation |
| Details | Progressive disclosure of location, size, modification time, availability and sync status |
| Transfers | Progress, pause/cancel/retry, useful error descriptions and expandable technical detail |
| Sync setup | Choose an empty local root and remote folders; show direction, estimated size, exclusions and deletion behavior before activation |
| Conflicts | Show local and remote versions; preserve both by default and make replacement choices explicit |
| Settings | Account, selected folders, background behavior, appearance, motion, cache limits and diagnostics |

Use sentence case and concise, credible copy such as “Everything in sync.” A completed initial scan is required before displaying that state. Distinguish “Checking for changes,” “Syncing,” “Paused,” “Offline,” and “Needs attention.” A failed or incomplete scan must never appear as an empty Drive.

Keep everyday file management flat and functional. Reserve atmospheric imagery and large typography for onboarding and marketing. Avoid idle glowing or pulsing effects; replace motion with static state when reduced motion is enabled. Respect text scaling, keyboard navigation, visible focus and screen-reader announcements. Use 4.5:1 normal-text and 3:1 large-text/control contrast as design acceptance targets.

## 5. Google Drive integration

### Authentication

Implement the installed-desktop OAuth flow using the system browser, a loopback callback bound to the local machine, PKCE S256 and validated `state`. Handle cancellation, timeout, expired access tokens, refresh failure and revoked access. A packaged desktop client cannot keep a client secret confidential. [Google desktop OAuth guide](https://developers.google.com/identity/protocols/oauth2/native-app)

Use a separate test project and disposable test account for development. Document configuration of the public distribution client, consent screen and required publisher information. Disconnect should stop work and remove local credentials while retaining user files; make remote grant revocation a clearly explained action.

### Scope requirements

For the planned file-management and bidirectional sync release, request **`https://www.googleapis.com/auth/drive`**. This is a **restricted** scope, justified by reading and modifying existing My Drive files. Do not also request redundant Drive scopes or unrelated Workspace scopes.

| Scope suffix under `https://www.googleapis.com/auth/` | Google classification | Aether use |
| --- | --- | --- |
| `drive` | Restricted | Planned production scope for full browsing, transfers, edits, moves, trash and bidirectional sync |
| `drive.readonly` | Restricted | Use for the read-only development milestone, or a separately shipped read-only product; insufficient for writes |
| `drive.metadata.readonly` | Restricted | Metadata-only access; insufficient for content downloads or sync and unnecessary alongside `drive` |
| `drive.file` | Non-sensitive | App-created or explicitly shared files; insufficient for the planned general existing-Drive browser and sync |

The classifications and access boundaries above come from [Google's Drive scope documentation](https://developers.google.com/workspace/drive/api/guides/api-specific-auth). Google lists backup and sync as a permitted restricted-scope use case. Folder selection inside Aether limits what Aether synchronizes; it does not narrow the account-wide OAuth grant. Explain this distinction in onboarding. The broad grant can permit actions beyond Aether's exposed UI; enforce the product's no-permanent-deletion rule in application code.

Use only the access required by the implemented milestone. When moving from a read-only grant to write-capable features, obtain a new consent grant for `drive` and verify the returned scopes before enabling writes. Do not assume a previous token gains permissions when Cloud Console configuration changes. Google does not support incremental authorization for installed apps; test the new authorization flow explicitly. [Desktop OAuth guidance](https://developers.google.com/identity/protocols/oauth2/native-app)

### Google verification and security assessment

**Policy baseline checked October 19, 2026; recheck before submission.** Distinguish Google OAuth approval from an independent security assessment:

| Requirement | Expected treatment for Aether |
| --- | --- |
| Brand verification | Required for the intended public distribution: consistent app identity, controlled domain, public homepage, privacy policy and support contacts |
| Restricted-scope verification | Required for the intended public distribution using `drive`; justify why `drive.file` cannot support the implemented features |
| Independent CASA security assessment | Google's criteria require assessment for restricted data accessible from or through a third-party server. The on-device architecture is intended to avoid that condition; confirm Google's determination during review |
| Recurring assessment | If Google determines an assessment is required, plan for its applicable CASA process and at least annual reassessment; do not treat approval as a one-time certification |

Google's restricted-scope guidance ties the third-party assessment to server access or the capability for that access. **Our expectation that Aether will not need an outside assessment is an architectural inference, not an approved exemption.** Record Google's determination and any conditions before public release. Do not book an assessor or claim certification based solely on the scope classification. [Restricted-scope verification and assessment criteria](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)

Prepare the following verification package during development and complete it once the requested features can be demonstrated:

1. A production Google Cloud project with a Desktop OAuth client, accurate branding and a verified owned domain, separate from development/testing.
2. A public homepage, privacy policy and support contact describing direct Google access, local storage, disconnection, local-data removal and applicable Limited Use commitments.
3. A feature-to-scope justification and demonstration video showing the consent flow and actual use of the requested restricted scope.
4. A data-flow diagram and endpoint inventory covering login, refresh, sync, previews, diagnostics and distribution, with evidence that Aether hosting cannot access Drive credentials or data.
5. Google's verification outcome and assessment determination, retained with the release records. Resolve follow-up requests and any required assessment before public distribution.

Google permits development/testing and certain personal-use exceptions to verification. Use explicitly configured test users during development and account for warning screens, user limits and limited refresh-token lifetimes; these exceptions are not the public-launch strategy. Submit production review when the implemented functionality and evidence are ready, starting preparation in Phase 0. [Verification preparation and exceptions](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification)

If Google requires an assessment, its Trust and Safety team directs the applicable CASA process. Assessment tier and cost must be established at that point; no fixed cost, approval date or certification is assumed in this plan. [Google security assessment guidance](https://support.google.com/cloud/answer/13465431)

### API behavior

- Paginate listings, request only needed fields and honor per-file capabilities before enabling actions.
- Key records by account and stable Drive file ID, never by display name alone. Model duplicate names, moves and shortcuts explicitly.
- Seed a change token before the initial listing, then replay subsequent changes to close the bootstrap race. Persist each applied page and its next cursor transactionally; continue from the saved cursor after restart. If a cursor becomes unusable, reconcile a new full inventory without treating missing partial results as deletions. [Drive change tracking](https://developers.google.com/workspace/drive/api/guides/manage-changes)
- Poll changes locally with adaptive backoff; do not introduce a hosted webhook receiver or relay.
- Use resumable uploads for large or interruptible transfers, persist upload progress securely and recover expired sessions. Check remote outcomes before retrying ambiguous creates. [Drive upload guide](https://developers.google.com/workspace/drive/api/guides/manage-uploads)
- Download ordinary file content and expose explicit export choices for Google Workspace documents. Exports are snapshots, not editable bidirectional representations of the original document. [Drive download and export guide](https://developers.google.com/workspace/drive/api/guides/manage-downloads)
- Retry transient failures with bounded exponential backoff and jitter; honor server retry guidance. Distinguish rate limits, authentication errors, permission loss, storage quota and permanent failures.

## 6. Synchronization and data integrity

The sync engine is the highest-risk part of the project. Implement it as a testable reconciliation system before exposing automatic writes to ordinary user accounts.

1. **Persist a baseline.** Record remote identity/version metadata, local path/fingerprint, last agreed state, selected-folder membership and pending operations. Compare local and remote changes against the baseline, not only against timestamps.
2. **Map names deterministically.** Persist collision-safe mappings for duplicate names, invalid path characters and case-folding collisions. Prevent traversal outside the sync root. Do not follow symlinks or recursively synchronize shortcuts in the first release; show unsupported items clearly.
3. **Observe and reconcile.** Debounce local filesystem events, recover watcher overflow with scans, and periodically reconcile remote changes. Detect atomic-save patterns and suppress feedback loops from engine-originated writes.
4. **Plan before mutation.** Use a durable queue with explicit queued, running, paused, retrying, blocked, completed and cancelled states. Serialize dependent operations and bound concurrency. Record enough intent to recover an interrupted operation without blindly duplicating it.
5. **Publish downloads safely.** Write temporary files on the destination filesystem, verify integrity where metadata permits, flush and atomically publish. Check for concurrent local edits before replacement. Do not replace a user's file with an incomplete download.
6. **Detect conflicts.** When both sides changed, preserve both and request resolution. Recheck remote state before uploading an overwrite; validate available conditional-write behavior in Phase 0. Do not claim atomic conflict protection until the API race cases are tested.
7. **Handle deletion conservatively.** Use remote trash and a documented, recoverable local trash/quarantine path. Permission loss, an unavailable mount, failed enumeration or deselection must not trigger deletion propagation. Pause unexpected bulk deletion for review. Never permanently delete remote content in the initial release.
8. **Recover deliberately.** Restart safely after network loss, sleep, process termination, disk-full errors and database migration failure. Do not declare success until remote/local effects and durable state agree.

Default sync setup should use an empty local directory to avoid accidental merging of unrelated files. Explain that edits and deletions inside selected folders propagate in both directions. Deselecting a folder stops synchronization and retains its existing local files; removing local copies is a separate explicit action. Pausing blocks new work and brings active operations to a documented safe boundary.

Google-native documents remain browser-openable items in the app; exclude one-way exports from automatic round-trip sync. Revoke or narrow access without destroying already-downloaded user files. After reconnecting, revalidate remote state before resuming queued writes.

## 7. Phased execution and exit criteria

### Phase 0 — Resolve foundational risks

**Dependencies:** Existing brand guidelines and reference image.

- Build a small GTK shell showing the proposed sidebar, file list, dialog and brand tokens.
- Validate Wayland rendering, fractional scaling, keyboard focus, screen-reader semantics and dark/light/high-contrast behavior on the target desktops.
- Prototype OAuth, credential storage, listing, change tracking and a resumable transfer with a disposable account.
- Test remote overwrite races and duplicate-name mappings; document supported protection and remaining limitations.
- Record architecture decisions for UI toolkit, sync semantics, background lifecycle and initial package format. Document the accepted on-device boundary, milestone scopes and endpoint inventory; start preparing the Google verification package.

**Exit:** A working technical spike and written decisions demonstrate a viable native UI and direct Drive connection. Review network destinations and credential storage to confirm that no Aether-hosted or other third-party service receives Google credentials or Drive data. Any unresolved data-integrity limitation has a concrete mitigation before sync implementation begins.

### Phase 1 — Foundation and branded shell

**Dependencies:** Phase 0 decisions.

- Create the Rust workspace, module boundaries, formatting/linting configuration and Linux CI.
- Introduce domain types, SQLite migrations, test fixtures, structured redacted diagnostics and an injectable Drive adapter.
- Build tokens, reusable controls and screens using fixture data, including loading, empty, error, offline and permission-denied states.
- Prepare production branding assets, launcher metadata and a keyboard-shortcut reference.

**Exit:** The app launches without credentials, supports keyboard-only navigation, and demonstrates the complete visual system at 100%, 150% and 200% scale in dark and light modes.

### Phase 2 — Authenticated, read-only browser

**Dependencies:** Phase 1 and the OAuth prototype.

- Add direct-to-Google sign-in, local credential persistence, refresh, reconnect and disconnect using the read-only milestone scope.
- Implement paginated My Drive navigation, sorting, cached metadata/search, details and browser launch for Google-native files.
- Establish initial indexing and incremental changes; identify stale/offline results clearly.

**Exit:** A test account can be browsed and searched across restarts. Revoked credentials, interrupted indexing and permission failures produce recoverable states without corrupting cached metadata.

### Phase 3 — Manual transfers and file actions

**Dependencies:** Phase 2.

- Implement upload, download, create-folder, rename, move and trash operations with capability checks.
- Obtain consent for `drive` when enabling write operations; verify the granted scope and handle refusal without enabling writes.
- Add the persistent transfer queue, progress, cancellation, retry and export choices.
- Implement safe local writes, duplicate-name handling and ambiguous-request recovery.

**Exit:** Transfers survive process restart and connection interruption. Cancelled or failed operations preserve existing files. Manual actions pass integration tests against a disposable Drive account. This is the first usable preview.

### Phase 4 — Selective synchronization

**Dependencies:** Phase 3 and validated conflict protections.

- Introduce the single-owner background service on the user's computer and same-user local IPC; keep all sync processing and state on-device.
- Build folder selection, initial reconciliation, baseline tracking, filesystem observation and remote polling.
- Implement pause/resume, exclusions, conflicts, deletion safeguards and recovery.
- Add a dry-run reconciliation view for development and automated scenario testing.

**Exit:** The engine converges in controlled edit/move/delete scenarios and preserves both versions of concurrent edits. Crash, disk-full, permission-loss, missing-root and mass-deletion cases pass before enabling beta sync on user-selected data.

### Phase 5 — Linux integration and release hardening

**Dependencies:** Phase 4.

- Add notifications, optional tray controls, file-manager reveal and opt-in start-at-login behavior.
- Provide full functionality when a desktop has no tray host; background state remains visible in the main app.
- Test suspend/resume, logout, shutdown, fractional scaling, IME input, screen readers and reduced motion on Arch/Omarchy, GNOME and KDE.
- Produce an Arch package and source-build instructions, upgrade/migration tests, checksums and release notes. Use package-manager upgrades initially.
- Publish onboarding, sync semantics, recovery instructions and privacy/data-handling documentation. Complete Google brand and restricted-scope verification, record Google's assessment determination, and satisfy any assessment requirement before public distribution.

**Exit:** The release checklist below passes, installation and upgrade work on the declared support matrix, and there are no known reproducible silent-loss or silent-overwrite defects in the tested workflows.

### Phase 6 — Expand after stable release

Prioritize from observed user needs: Flatpak, multiple accounts, shared drives, richer search and previews, and file-manager integration. Preserve the on-device boundary for every capability. Treat virtual mounts and placeholder hydration as separate architectural projects. Add new capabilities only after extending the sync, recovery and data-boundary test matrix for them.

## 8. Verification and release checklist

| Area | Required evidence |
| --- | --- |
| Brand and UX | Screenshots of onboarding, browser, transfer queue, conflict dialog and settings match the brand; no decorative imagery obstructs routine file work |
| Accessibility | Keyboard-only task completion, visible focus, screen-reader labels/state updates, contrast review, text scaling and reduced motion |
| Drive correctness | Pagination, duplicate names, shortcuts, permission changes, Google-native documents, rate limits, refresh failures and resumable sessions |
| Sync correctness | Baseline-based conflict scenarios, rename/move/delete sequences, root disappearance, watcher overflow and randomized event ordering |
| Recovery | Fault injection before/after local writes, API responses, queue commits and cursor commits; restart produces no silent overwrite or duplicate side effect |
| Persistence | Tested migrations and recovery path; cache clearing never removes synced user files or credentials |
| Credential/privacy handling | Local credential storage; no tokens, authorization URLs, file contents or unredacted sensitive paths in default logs; diagnostics export is local, user initiated and redacted |
| On-device boundary | Network destination and payload review across login, refresh, sync, previews, errors and shutdown shows Google credentials/Drive data go only to Google; no hosted proxy, webhook, analytics or crash-upload path |
| Google authorization | Milestone scopes match implemented features; refusal and reauthorization tested; production brand/scope verification and assessment determination documented before public release |
| Packaging | Clean install, upgrade, uninstall, launcher, service lifecycle and no-tray operation; uninstall preserves user files |

Set performance budgets in Phase 0 on a documented reference machine and dataset. Initial engineering targets: cached launch within 2 seconds, cached filename search p95 within 150ms over 100,000 metadata entries, smooth scrolling of a 10,000-row virtualized folder, and no sustained idle polling loop. Measure transfer workloads separately from idle operation and record CPU, memory, request rate and wakeups. These are proposed targets, not marketing claims or measured results.

Use unit tests for reconciliation decisions and path mapping, adapter contract tests with a simulated Drive service, fault-injection tests for recovery, and opt-in integration tests with disposable accounts. Run live Google integration tests only on a developer's computer with local credentials and artifacts; hosted CI uses synthetic fixtures and simulated services, never real Drive credentials or downloaded data. Inspect network destinations and outgoing payloads without retaining tokens or content in test reports. Run native UI verification on real desktops as well as CI. Prefer scenario and invariant coverage over a superficial coverage percentage.

## 9. Decisions to close before implementation expands

| Decision | Default direction | Resolve by |
| --- | --- | --- |
| UI toolkit and branding feasibility | Rust + GTK4/libadwaita with semantic styling | Phase 0 exit |
| Public OAuth project ownership and verification | Dedicated Aether project; `drive` for the write-capable release; complete brand and restricted-scope review | Prepare in Phase 0; complete before public release |
| Google's security-assessment determination | Present evidence of direct, on-device access; expect no server-triggered assessment, subject to Google's review | Confirm and satisfy any requirement before public release |
| Conditional overwrite guarantees | Test the actual API behavior; preserve versions when uncertain | Before Phase 4 |
| Light palette and accessible status colors | Derive from the brand and validate visually | Phase 1 exit |
| Local deletion recovery and retention | Recoverable quarantine/trash with a documented retention setting | Before Phase 4 |
| Background behavior | Explicit opt-in; service owns sync, UI can close independently | Before Phase 4 |
| Distribution support matrix | Arch/Omarchy first; GNOME/KDE compatibility; Flatpak later | Before Phase 5 |

The immediate next implementation task is the Phase 0 native shell and Drive integration spike. Update this document with evidence and accepted decisions as each gate closes; keep [brand-guidelines.md](brand-guidelines.md) authoritative for UI and branding.
