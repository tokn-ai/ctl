# Desktop feedback placement

Messages follow the action or content that owns them. A field, form, row, or
section keeps its validation and recovery nearby. A standalone operation or an
unexpected background event uses a bottom-right notification backed by the same
notification-center entry. Session connection and error states use a translucent
floating banner inside the owning session area, below the tab bar. The state,
diagnostic, and reconnect or other recovery action stay together in that banner
and follow the selected session. Global popups and the notification center also
float outside the main layout. Showing or hiding these surfaces never resizes
or shifts the main panel.

## Message inventory

| Messages | Placement and recovery |
| --- | --- |
| Invalid command quoting, executable, argument, task name, or working directory | Beside the affected field in the task editor. Keep the draft open. |
| Invalid VPN name, endpoint, credentials, or connection settings | Beside the affected field or in the VPN editor form. Keep corrections local. |
| Missing update hosts or component-bundle path | Beside the affected selection or path in Update components. |
| SSH destination, authentication, route, host settings, and forwarding form errors | In the active input flow or settings dialog, beside the relevant control where possible. |
| Task definition save, delete, reload, and revision conflicts | In the task editor, with the current draft and conflict actions retained. |
| Incorrect identity-file passphrase | In the passphrase prompt so the value can be corrected. |
| Session-list or host-check failures | In the session sidebar or affected host, with Retry session list or Retry host check. Retain last-known sessions. |
| Task-list or saved-definition refresh failures | In the matching Tasks sidebar section, with its retry action and last-known data. |
| Active task definition, terminal lookup, or output-log failures | In that task's detail view, with Retry definition, Retry terminal, or Retry logs. |
| VPN status/catalog checks, incomplete inventory, or stale observations | In the VPN section. Keep prior status visible and offer Retry VPN check. |
| VPN connect, disconnect, remove, and enrollment failures | On the affected connection, editor, or enrollment view. Browser-opening failures retain Open browser there. |
| Port-forward status or listener-scan failures | In the forwarding section or listener picker, with the corresponding retry action. |
| Start/stop forwarding failure from the management dialog | On the affected forwarding row, with Retry forward. Preserve unrelated form validation. |
| Credential/identity inventory failures, incomplete sources, and Keychain limitations | In the matching Credentials section. Retain the last successful inventory. |
| Saved-metadata import outcomes | At the import action in Credentials. |
| Component version checks, host probes, restart preflight failures, bundle catalog/activation checks, and published-build limitations | In the relevant About section, host, or component row. |
| Component update progress and per-host results | In the retained Update components dialog. Do not repeat the outcome as a popup. |
| Windows manual setup and unsupported maintenance, VPN, or listener operations | In the owning Connect host flow, About host row, VPN view, or forwarding dialog. Present persistent capability limits as context; hide unavailable actions. |
| Workspace load failure | A workspace error view with Retry loading. |
| Workspace save failure | An error in the notification center with Retry saving. Do not show a banner or popup; resolve the entry after saving succeeds. |
| Saving, connecting, reconnecting, enrollment, layout/input ownership, and maintenance progress | In the owning view or status line. Routine progress does not create notifications. |
| Input or layout refusal and pane-layout refresh failure | In the input/layout controls or terminal area, with Retry layout where applicable. |
| Disconnected saved tab, authentication required, and intentional disconnect/cancellation | In the owning session banner or connection flow, with the relevant local connect action. No popup for an expected state. |
| Missing session and normal session exit | In the terminal, with the relevant close or dismiss action. No popup for an expected state. |
| Earlier terminal output unavailable | In terminal status and a silent notification-center entry. Archive history gaps stay in the archive view. |
| Unexpected session connection failure or loss | A floating banner inside the owning session area with the diagnostic and Reconnect or another supported recovery action. Keep the tab bar visible. One popup and center entry may announce the incident; retain manual Windows setup/update guidance when automated maintenance is unavailable. |
| Proven SSH connection loss without an attachment already owning recovery | One popup and center entry for the host, with Connect host. Polling errors alone do not prove connection loss. |
| Newly failed task run | One popup and center entry with View task. Existing failed runs found on initial load do not create popups. |
| Standalone command, pane operation, session action, clipboard, or link-opening failure | One popup and center entry. A command whose form or retained result view owns the failure keeps it there instead. |
| Credential/passphrase removal failure after confirmation; non-correctable passphrase-save failure | One popup and center entry after the initiating prompt closes. |
| Port-forward start/stop from the sidebar or automatic restore failure | One popup and center entry. Status refresh failures remain in the forwarding section. |
| Standalone task commands and task/component daemon restart outcomes | One popup and center entry after the initiating confirmation closes. |
| Invalid keyboard shortcuts, session-history storage failure, component-event subscription failure, or failed window closure | One popup and center entry with an available recovery action. Preserve valid shortcut bindings and existing data. |
| Destructive actions, restart impact, trust/authentication consent, and overwrite/conflict decisions | A confirmation or input dialog requiring an explicit response. Never use a toast as the decision surface. |

## Notification lifecycle

Each popup and its center entry represent the same occurrence. Repeated polling
or changing automatic-retry diagnostics update an existing incident without
restoring a hidden or dismissed popup. An explicit retry may create a new
occurrence. Proven recovery resolves the retained entry and removes stale
actions. Notifications are window-local and their contents are not persisted.

Use `InlineFeedback` for owned feedback, `NotificationStore.report` for observed
conditions, and `NotificationStore.notify` for distinct operation outcomes.
Keep placement decisions at the feature that knows whether an operation has a
retained form or result view. Backend error wording does not determine placement.
