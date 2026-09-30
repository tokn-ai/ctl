# Connection observations and transitions

Connection state is runtime evidence, not a persisted property of a saved host
or terminal. These independent observations must remain separate:

| Observation | Evidence | What it does not establish |
| --- | --- | --- |
| SSH connected | The local OpenSSH control master answered `-O check` | Remote reachability or terminal attachment health |
| Remote connection timed out | Opening the remote rmux service timed out while a local SSH master remains available | Authentication failure, confirmed SSH disconnection, or remote process exit |
| SSH available | The configured route returned an SSH identification greeting during a brief probe | Successful authentication, verified host identity, or an established SSH session |
| Terminal attached | This attachment completed its protocol handshake and has not reported closure/failure | Other terminals or connection methods are healthy |
| Session ended | An explicit session-ended event or confirmed missing-session response | A transport failure alone never proves process exit |

## Hosts

`HostConnectionObservation` contains availability, observation completeness,
method names, failed methods, and the observation time. `HostConnectionOperation`
separately describes the current connect/disconnect attempt and its outcome.
The hook publishes both; its legacy `state` field is a derived compatibility
summary, not another authority.

- A positive result from any method establishes SSH availability for that method.
- Failed probes for other methods make the observation partial. The row still
  shows SSH connected; failed checks remain in its tooltip.
- Unavailability requires successful negative observations for every method.
  Explicit manual pause is reported separately: it prevents use in this app,
  without claiming that an externally owned control master disappeared.
- A timed-out or failed observation means unknown, never disconnected.
- A failed alternate connection attempt does not erase an available method.
- A settings change invalidates both published evidence and results still in flight.
- Manual disconnect pauses terminal recovery separately from whether a shared
  OpenSSH master still exists. A native observation spanning a disconnect/reconnect
  generation cannot establish the replacement's state.

Master-status queries inspect the local SSH control connection. Reachability
checks are separate: they open a brief connection to the configured SSH port,
verify its [SSH identification greeting](https://www.rfc-editor.org/rfc/rfc4253#section-4.2),
and close before authentication or session creation. A successful probe keeps
the Connect action available; it never grants Disconnect controls or resumes
manually paused sessions and forwards.

Reachability checks use direct routes or existing VPN and unauthenticated SOCKS5
routes. They never start a VPN, authenticate to an SSH gateway, request proxy
credentials, or fall back to a direct connection when a route cannot be used.
An inactive VPN and unsupported routes are reported as not checked, rather than
evidence that the remote SSH service is down. SSH configuration is read without
executing custom commands; unsupported routing or dynamic configuration also
leaves reachability unverified.
Static configuration inspection currently supports standard OpenSSH locations
on macOS and Linux. Other platforms report the probe as not checked.

Only current configured methods are probed. Retained previous routes are checked
for existing masters, but cannot establish availability for a new connection.
Reachability refreshes are throttled separately from master polling and are
invalidated by host settings or VPN route changes. Each query has a bounded wait;
late results cannot update the snapshot. Tooltips identify the check time and
the limits of this evidence.
An active remote-service timeout takes precedence over the connected label;
the underlying master observation and Disconnect action remain available.
Existing-master lookup and remote-service startup have separate ten-second
deadlines. Neither timeout requests authentication; only explicit authentication
errors offer Connect host. A successful session inspection or attachment retires
its error card and marks the retained notification as resolved.
Unavailable Tailscale routes use the compact label Tailscale unavailable, with
the full reason in the tooltip or connection details. An unavailable preferred
route does not hide SSH availability observed through another method.

## Terminal attachments

`transitionAttachment` owns lifecycle transitions. The connection generation
fences async completions and channel events before they enter the transition
function. React displays a synchronous runtime snapshot so multiple events in a
single batch cannot read an obsolete lifecycle state.

| Current state | Event | Next state |
| --- | --- | --- |
| Any | Explicit attach intent | Connecting |
| Attached / interrupted / failed | Reconnect intent | Reconnecting |
| Connecting / reconnecting | Successful open response | Attached |
| Attached | Transport closed | Disconnected |
| Opening / attached | Fatal attachment failure | Error |
| Disconnected / recoverable error | Retry scheduled | Waiting to retry, with a deadline |
| Waiting to retry | Timer fires within the recovery budget | Reconnecting |
| Waiting to retry / recoverable failure | Budget exhausted | Error, awaiting explicit retry |
| Attached | Confirmed process exit | Ended |
| Ended | Trailing transport close or failure | Ended |
| Any | Explicit detach/reset | Idle (or an explicit detach failure) |

Replay cursors, including `0` and a missing checkpoint, select data recovery.
They never decide whether an operation is an initial connection or a reconnect.
Waiting owns one timer; opening owns one attempt. The existing 30-second budget
bounds the scheduling of automatic retries, not the duration of an already
running native open. Resizes and checkpoint updates do not reset stability time.

Only Attached permits live activity or owned input/layout leases. Failure fences
the old actor immediately and releases its attachment. An open that succeeds
after supersession, or cannot be adopted by the renderer, is detached by its
exact ID. No connection-state transition kills a remote shell.

Manual Reconnect first checks the attachment's exact SSH route. If its master
is absent or manually disconnected, the existing Connect host flow authenticates
that route before retrying the original terminal. A connected master is reused;
an unknown status is reported without assuming authentication is needed. Closing
the flow or the original attachment cancels the pending retry. Background tabs
and split panes keep their own reconnect intent without selecting another tab.
Automatic recovery and component-restart recovery do not open authentication
dialogs. If the master disappears between a successful status check and opening
the terminal, a concrete authentication-required result permits one host-connection
attempt before the manual retry finishes.

Cached output, shell activity, and session-list observations remain available
while offline, with last-known wording and neutral status. An unreachable
transport does not rewrite the last observed remote process as exited.

The attachment peer-silence deadline runs independently of outbound writes, so
a blocked SSH pipe cannot suppress failure detection. A failed write closes the
input path but still allows buffered output and a confirmed terminal-exit message
to arrive. This read drain is bounded by one negotiated peer timeout even if the
peer keeps sending data; a write failure alone never proves the shell exited.
Private SSH masters started by ctld also use a ten-second server-alive interval
with three unanswered probes.
Configured shared masters retain their owner's keepalive policy. New settings
apply to newly started masters; existing ones are not restarted automatically.

This foundation precedes new retry/cancel controls. Those controls must consume
these transitions, preserve the session/cache, and keep cancellation separate
from host-wide disconnect. Cancellation during backoff and native queueing must
be resolved before exposing new cancellation actions.
