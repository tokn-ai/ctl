import { useLocale } from "../../features/i18n/LocaleContext";
import { useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { SshHostFlow, type SshHostFlowProps } from "./SshHostFlow";
import { hostTarget } from "../../features/workspace/workspaceModel";
import { connectionUnavailableLabel } from "../../features/targets/targets";
import { errorMessage } from "../../lib/errors";
import type { ConnectionTarget, WorkspaceConnectionMethod, WorkspaceHost } from "../../lib/types";

interface ConnectHostFlowProps extends Omit<SshHostFlowProps, "target" | "autoConnect"> {
  target: ConnectionTarget;
  host?: WorkspaceHost;
  /** A method already chosen explicitly, such as Connect in host settings. */
  selected_method_id?: string;
}

/** Choose a route only when there is a choice, then enter SSH verification. */
export function ConnectHostFlow({
  target,
  host,
  selected_method_id,
  gateways = [],
  ...props
}: ConnectHostFlowProps) {
  const { t, locale } = useLocale();
  const [chosenMethodId, setChosenMethodId] = useState<string | null>(() =>
    host?.connection_methods.length === 1 ? host.connection_methods[0].method_id : null);
  const method_id = selected_method_id ?? chosenMethodId ??
    (host?.connection_methods.length === 1 ? host.connection_methods[0].method_id : undefined);
  let candidate = target;
  let unavailable = target.kind === "ssh" && (!host || props.updateRequired) ? target.unavailable : undefined;
  if (host && !props.updateRequired) {
    if (host.source === "unavailable") {
      unavailable = t("host_connect.missing");
    } else if (!host.connection_methods.length) {
      unavailable = t("host_connect.no_methods");
    } else if (method_id) {
      if (!host.connection_methods.some((method) => method.method_id === method_id)) {
        unavailable = t("host_connect.missing_method");
      } else {
        try {
          candidate = hostTarget(host, gateways, method_id, props.hosts);
          if (candidate.kind === "ssh") unavailable = candidate.unavailable;
        } catch (failure) {
          unavailable = errorMessage(failure, locale);
        }
      }
    }
  }

  if (unavailable) {
    return (
      <QuickInput
        key="unavailable"
        title={t("host_connect.unavailable")}
        error={unavailable}
        mode={{ kind: "pick", choices: [{ id: "close", label: t("host_connect.close") }] }}
        onSubmit={props.onClose}
        onCancel={props.onClose}
      />
    );
  }

  if (host && !method_id && !props.updateRequired) {
    const preferred = host.connection_methods.find((method) => method.method_id === host.preferred_method_id);
    const methods = preferred
      ? [preferred, ...host.connection_methods.filter((method) => method !== preferred)]
      : host.connection_methods;
    return (
      <QuickInput
        key={`methods:${host.host_id}`}
        title={t("host_connect.connect_to", { host: host.name })}
        description={t("host_connect.choose_method")}
        mode={{
          kind: "pick",
          choices: methods.map((method) => ({
            id: method.method_id,
            label: method.name,
            detail: [
              method.method_id === host.preferred_method_id ? t("host_settings.preferred") : null,
              methodEndpoint(method),
              method.target.unavailable ? connectionUnavailableLabel(method, locale) : null,
            ].filter(Boolean).join(" · "),
          })),
        }}
        onSubmit={setChosenMethodId}
        onCancel={props.onClose}
      />
    );
  }

  return <SshHostFlow key={method_id ?? "target"} {...props} target={candidate} gateways={gateways} autoConnect />;
}

function methodEndpoint(method: WorkspaceConnectionMethod): string {
  const target = method.target;
  const hostname = target.hostname ?? target.destination;
  const address = hostname.includes(":") ? `[${hostname}]` : hostname;
  return `${target.user ? `${target.user}@` : ""}${address}${target.port ? `:${target.port}` : ""}`;
}
