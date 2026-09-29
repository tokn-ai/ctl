import type {
  HostConnectionObservation,
  HostConnectionOperation,
  HostConnectionStatus,
  SshConnectionStatus,
} from "../../lib/types";

export interface HostConnectionModel {
  observation: HostConnectionObservation;
  operation: HostConnectionOperation;
  manually_disconnected: boolean;
  disconnect_incomplete: boolean;
}

export interface MethodObservation {
  name: string;
  status: SshConnectionStatus | null;
  error: string | null;
}

export interface HostObservationResult {
  observation: HostConnectionObservation;
  manually_disconnected: boolean;
  disconnect_incomplete: boolean;
  usable_connection: boolean;
}

const idleOperation = (): HostConnectionOperation => ({
  kind: null, state: "idle", method_name: null, message: null,
});

export function emptyHostConnection(stale = false): HostConnectionModel {
  return {
    observation: {
      availability: "unknown", completeness: "pending", method_names: [],
      failed_method_names: [], message: null, checked_at_ms: null, stale,
    },
    operation: idleOperation(),
    manually_disconnected: false,
    disconnect_incomplete: false,
  };
}

/** Failed queries establish no absence; any positive observation establishes availability. */
export function aggregateHostObservations(methods: readonly MethodObservation[], checked_at_ms: number): HostObservationResult {
  const connected = methods.filter((method) => method.status?.connected);
  const failures = methods.filter((method) => !method.status);
  const usable_connection = connected.some((method) => !method.status?.manually_disconnected);
  const complete = methods.length > 0 && failures.length === 0;
  const manually_disconnected = complete && !usable_connection &&
    methods.some((method) => method.status?.manually_disconnected);
  return {
    observation: {
      availability: connected.length ? "available" : complete ? "unavailable" : "unknown",
      completeness: complete ? "complete" : failures.length === methods.length ? "failed" : "partial",
      method_names: connected.map((method) => method.name),
      failed_method_names: failures.map((method) => method.name),
      message: methods.length ? [...new Set(failures.map((method) => method.error ?? "SSH status could not be checked."))].join("; ") || null
        : "No SSH connection methods are available.",
      checked_at_ms,
      stale: false,
    },
    manually_disconnected,
    disconnect_incomplete: manually_disconnected && connected.length > 0,
    usable_connection,
  };
}

export function observeHostConnection(model: HostConnectionModel, result: HostObservationResult): HostConnectionModel {
  return {
    ...model,
    observation: result.observation,
    manually_disconnected: result.manually_disconnected,
    disconnect_incomplete: result.disconnect_incomplete,
  };
}

export function beginHostOperation(model: HostConnectionModel, kind: "connect" | "disconnect", method_name: string | null): HostConnectionModel {
  return { ...model, operation: { kind, state: "pending", method_name, message: null } };
}

export function failHostOperation(model: HostConnectionModel, kind: "connect" | "disconnect", message: string): HostConnectionModel {
  return { ...model, operation: { ...model.operation, kind, state: "failed", message } };
}

export function finishHostConnection(model: HostConnectionModel, connected: boolean): HostConnectionModel {
  return {
    ...model,
    operation: idleOperation(),
    ...(connected ? { manually_disconnected: false, disconnect_incomplete: false } : {}),
  };
}

export function finishHostDisconnect(checked_at_ms: number): HostConnectionModel {
  const model = emptyHostConnection();
  return {
    ...model,
    observation: { ...model.observation, availability: "unavailable", completeness: "complete", checked_at_ms },
    manually_disconnected: true,
  };
}

/** Compatibility state summarizes evidence; operation never overwrites available transport. */
export function hostConnectionStatus(model: HostConnectionModel): HostConnectionStatus {
  const { observation, operation } = model;
  const disconnect_failed = operation.kind === "disconnect" && operation.state === "failed";
  const state: HostConnectionStatus["state"] = operation.kind === "disconnect" && operation.state === "pending" ? "disconnecting"
    : disconnect_failed || model.disconnect_incomplete ? "error"
    : observation.availability === "available" ? "connected"
    : operation.kind === "connect" && operation.state === "pending" ? "connecting"
    : operation.state === "failed" || observation.completeness === "failed" || observation.completeness === "partial" ? "error"
    : observation.availability === "unavailable" ? "disconnected" : "checking";
  const message = operation.message ?? observation.message ?? (
    model.disconnect_incomplete ? "The SSH connection is still open after a disconnect. Retry Disconnect host or connect again."
      : model.manually_disconnected && operation.state === "idle" ? "Disconnected manually. Connect this host to resume." : null
  );
  return { state, method_names: observation.method_names, message,
    manually_disconnected: model.manually_disconnected, observation, operation };
}
