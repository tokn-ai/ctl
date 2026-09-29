# Connection observations and transitions

Connection state is runtime evidence, not a persisted property of a saved host
or terminal. Three independent observations must remain separate:

| Observation | Evidence | What it does not establish |
| --- | --- | --- |
| SSH available | The local OpenSSH control master answered `-O check` | Remote reachability or terminal attachment health |
| Terminal attached | This attachment completed its protocol handshake and has not reported closure/failure | Other terminals or connection methods are healthy |
| Session ended | An explicit session-ended event or confirmed missing-session response | A transport failure alone never proves process exit |

## Hosts

`HostConnectionObservation` contains availability, observation completeness,
method names, failed methods, and the observation time. `HostConnectionOperation`
separately describes the current connect/disconnect attempt and its outcome.
The hook publishes both; its legacy `state` field is a derived compatibility
summary, not another authority.

- A positive result from any method establishes SSH availability for that method.
- Failed probes for other methods make the observation partial. They remain visible.
- Unavailability requires successful negative observations for every method.
  Explicit manual pause is reported separately: it prevents use in this app,
  without claiming that an externally owned control master disappeared.
- A timed-out or failed observation means unknown, never disconnected.
- A failed alternate connection attempt does not erase an available method.
- A settings change invalidates both published evidence and results still in flight.
- Manual disconnect pauses terminal recovery separately from whether a shared
  OpenSSH master still exists. A native observation spanning a disconnect/reconnect
  generation cannot establish the replacement's state.

Status queries are passive. They do not authenticate or contact a remote host.
Each frontend query has a bounded wait; late results cannot update the snapshot.
The tooltip identifies the check time and the limits of this evidence.

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

Cached output, shell activity, and session-list observations remain available
while offline, with last-known wording and neutral status. An unreachable
transport does not rewrite the last observed remote process as exited.

This foundation precedes new retry/cancel controls. Those controls must consume
these transitions, preserve the session/cache, and keep cancellation separate
from host-wide disconnect. Cancellation during backoff and native queueing must
be resolved before exposing new cancellation actions.
