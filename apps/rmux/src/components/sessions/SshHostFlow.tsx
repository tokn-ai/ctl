import { useEffect, useRef, useState } from "react";
import { QuickInput, type QuickInputMode } from "../commands/QuickInput";
import { parseHostAddress } from "../../features/targets/hostAddress";
import { useSshIdentityFiles } from "../../features/targets/useSshIdentityFiles";
import {
  appLocalSshTarget,
  configuredSshTarget,
} from "../../features/targets/targets";
import { errorMessage } from "../../lib/errors";
import {
  cancelSshProbe,
  forgetSshCredentials,
  probeSshHost,
  respondSshPrompt,
} from "../../lib/tauri";
import type {
  ConnectionTarget,
  SshHostDefinition,
  SshHostStorage,
  SshPrompt,
} from "../../lib/types";

interface SshHostFlowProps {
  suggestions: readonly string[];
  warning: string | null;
  target?: ConnectionTarget;
  onActivateHost(destination: string): boolean;
  onSaveHost(
    definition: SshHostDefinition,
    storage: SshHostStorage,
  ): Promise<void>;
  onConnected(): void;
  onClose(): void;
}

type Step =
  | "host"
  | "name"
  | "auth"
  | "identity"
  | "progress"
  | "storage"
  | "retry"
  | "reconnect";

export function SshHostFlow({
  suggestions,
  warning,
  target,
  onActivateHost,
  onSaveHost,
  onConnected,
  onClose,
}: SshHostFlowProps) {
  const [step, setStep] = useState<Step>(target ? "reconnect" : "host");
  const identityFiles = useSshIdentityFiles(step === "identity");
  const [address, setAddress] = useState("");
  const [definition, setDefinition] = useState<SshHostDefinition>({
    alias: "",
    hostname: "",
    user: null,
    port: null,
    identity_file: null,
  });
  const [error, setError] = useState<string | null>(null);
  const [prompt, setPrompt] = useState<SshPrompt | null>(null);
  const [saving, setSaving] = useState(false);
  const attemptRef = useRef<string | null>(null);
  const candidateRef = useRef<ConnectionTarget | null>(target ?? null);
  const configuredRef = useRef(false);
  const closedRef = useRef(false);
  const uncommittedTargetRef = useRef<ConnectionTarget | null>(null);

  function forgetUncommitted() {
    const candidate = uncommittedTargetRef.current;
    uncommittedTargetRef.current = null;
    if (candidate) void forgetSshCredentials(candidate).catch(() => undefined);
  }

  function cancelAttempt() {
    const attempt = attemptRef.current;
    attemptRef.current = null;
    if (attempt) void cancelSshProbe(attempt).catch(() => undefined);
  }

  useEffect(() => {
    closedRef.current = false;
    return () => {
      closedRef.current = true;
      cancelAttempt();
      forgetUncommitted();
    };
  }, []);

  function close() {
    if (saving) return;
    closedRef.current = true;
    cancelAttempt();
    forgetUncommitted();
    onClose();
  }

  async function connect(candidate: ConnectionTarget) {
    cancelAttempt();
    forgetUncommitted();
    candidateRef.current = candidate;
    const attempt = crypto.randomUUID();
    attemptRef.current = attempt;
    setError(null);
    setPrompt(null);
    setStep("progress");
    try {
      await probeSshHost(candidate, attempt, (next) => {
        if (attemptRef.current === attempt && !closedRef.current)
          setPrompt(next);
      });
      if (attemptRef.current !== attempt || closedRef.current) return;
      setPrompt(null);
      if (target) {
        onConnected();
        onClose();
      } else if (configuredRef.current && candidate.kind === "ssh") {
        if (!onActivateHost(candidate.destination))
          throw new Error("That SSH host is already active.");
        onClose();
      } else {
        uncommittedTargetRef.current = candidate;
        setStep("storage");
      }
      attemptRef.current = null;
    } catch (failure) {
      if (attemptRef.current !== attempt || closedRef.current) return;
      attemptRef.current = null;
      setPrompt(null);
      setError(errorMessage(failure));
      setStep("retry");
    }
  }

  function connectDefinition(next = definition) {
    const candidate = appLocalSshTarget(next);
    if (candidate) void connect(candidate);
  }

  function answer(value: string) {
    const attempt = attemptRef.current;
    if (!attempt || !prompt) return;
    const promptId = prompt.prompt_id;
    const response = prompt.kind === "confirm" ? "yes" : value;
    setPrompt(null);
    void respondSshPrompt(attempt, promptId, response).catch((failure) => {
      if (attemptRef.current === attempt) {
        cancelAttempt();
        setError(errorMessage(failure));
        setStep("retry");
      }
    });
  }

  async function save(storage: SshHostStorage) {
    setSaving(true);
    setError(null);
    try {
      await onSaveHost(definition, storage);
      uncommittedTargetRef.current = null;
      if (!closedRef.current) onClose();
    } catch (failure) {
      if (!closedRef.current) setError(errorMessage(failure));
    } finally {
      if (!closedRef.current) setSaving(false);
    }
  }

  if (prompt)
    return (
      <QuickInput
        key={prompt.prompt_id}
        title={
          prompt.kind === "confirm"
            ? "SSH host verification"
            : "SSH authentication"
        }
        description={prompt.message}
        mode={
          prompt.kind === "confirm"
            ? { kind: "confirm", confirm_label: "Trust and connect" }
            : { kind: "input", label: "SSH response", secret: true }
        }
        onSubmit={answer}
        onCancel={close}
      />
    );

  let title = "Add host";
  let description: string | undefined;
  let mode: QuickInputMode;
  let onBack: (() => void) | undefined;
  const back = (previous: Step) => () => {
    setError(null);
    setStep(previous);
  };
  switch (step) {
    case "host":
      title = "Add host · 1/3";
      description =
        "Enter [user@]hostname[:port], or choose an SSH config host." +
        (warning ? `\n${warning}` : "");
      mode = {
        kind: "input",
        label: "SSH host",
        placeholder: "rmux@127.0.0.1:2222",
        initial_value: address,
        suggestions: suggestions.length
          ? {
              label: "SSH config hosts",
              items: suggestions.map((host) => ({
                id: `ssh-config:${host}`,
                label: host,
              })),
              empty_message: "Enter a hostname to add a new host.",
              no_match_message:
                "No matching SSH config hosts. Enter a hostname to add a new host.",
            }
          : undefined,
      };
      break;
    case "name":
      title = "Host name · 2/3";
      mode = {
        kind: "input",
        label: "Name / SSH alias",
        initial_value: definition.alias,
      };
      onBack = back("host");
      break;
    case "auth":
      title = "Authentication · 3/3";
      description =
        "OpenSSH authenticates this host. Passwords and key passphrases stay in memory for this app process only.";
      mode = {
        kind: "pick",
        choices: [
          {
            id: "default",
            label: "SSH config / agent",
            detail: "Use existing keys and SSH settings.",
          },
          {
            id: "identity",
            label: "Identity file",
            detail: "Specify a private-key path; never copy the key.",
          },
          {
            id: "password",
            label: "Password / interactive authentication",
            detail: "Answer OpenSSH's masked prompt when requested.",
          },
        ],
      };
      onBack = back("name");
      break;
    case "identity":
      title = "Identity file";
      description =
        "Choose a file from ~/.ssh using ↑/↓ and Enter, or type any private-key path. Key contents are not read for suggestions.";
      mode = {
        kind: "input",
        label: "Identity file",
        initial_value: definition.identity_file ?? "",
        placeholder: "~/.ssh/id_ed25519",
        suggestions: {
          label: "Identity files in ~/.ssh",
          items: identityFiles.identity_files.map((file) => ({
            id: file.path,
            label: file.display_path,
          })),
          loading: identityFiles.loading,
          loading_message: "Loading identity files…",
          empty_message:
            "No identity-file candidates in ~/.ssh. Enter a path manually.",
          no_match_message:
            "No matching identity files. Enter a path manually.",
          warning: identityFiles.warnings.join("\n") || undefined,
        },
      };
      onBack = back("auth");
      break;
    case "storage":
      title = "Save host";
      description = "Connection verified. Where should this host be saved?";
      mode = saving
        ? { kind: "progress", message: "Saving host…" }
        : {
            kind: "pick",
            choices: [
              {
                id: "ssh_config",
                label: "OpenSSH config",
                detail: "Reusable by ssh, ctl, and rmux-app.",
              },
              {
                id: "local_storage",
                label: "This app only",
                detail: "Store non-secret connection settings locally.",
              },
            ],
          };
      if (!saving) onBack = back("auth");
      break;
    case "reconnect":
    case "retry":
      title = step === "retry" ? "Could not connect" : "Connect host";
      description =
        "OpenSSH will ask for host verification or authentication if needed. ctld must be on the remote PATH.";
      mode = { kind: "pick", choices: [{ id: "retry", label: "Connect" }] };
      if (!target) onBack = back(configuredRef.current ? "host" : "auth");
      break;
    case "progress":
      title = "Connecting to host";
      description = "Starting the fixed remote command: exec ctld connect";
      mode = { kind: "progress" };
  }

  function submit(value: string) {
    setError(null);
    if (step === "host") {
      if (value.startsWith("ssh-config:")) {
        const candidate = configuredSshTarget(
          value.slice("ssh-config:".length),
        );
        if (candidate) {
          configuredRef.current = true;
          void connect(candidate);
        }
        return;
      }
      const parsed = parseHostAddress(value);
      if (!parsed) {
        setError(
          "Use [user@]hostname[:port], with IPv6 addresses in brackets. SSH flags are not accepted.",
        );
        return;
      }
      configuredRef.current = false;
      setAddress(value);
      setDefinition({ ...parsed, alias: parsed.hostname, identity_file: null });
      setStep("name");
    } else if (step === "name") {
      const alias = value.trim();
      if (!/^[a-zA-Z0-9_.:-]+$/u.test(alias) || alias.startsWith("-")) {
        setError("Enter a name without spaces or SSH patterns.");
        return;
      }
      setDefinition((current) => ({ ...current, alias }));
      setStep("auth");
    } else if (step === "auth") {
      if (value === "identity") setStep("identity");
      else {
        const next = { ...definition, identity_file: null };
        setDefinition(next);
        connectDefinition(next);
      }
    } else if (step === "identity") {
      const identity_file = value.trim();
      if (!identity_file || /[\x00-\x1f\x7f]/u.test(identity_file)) {
        setError("Enter a valid private-key path.");
        return;
      }
      const next = { ...definition, identity_file };
      setDefinition(next);
      connectDefinition(next);
    } else if (step === "storage") {
      if (!saving && (value === "ssh_config" || value === "local_storage"))
        void save(value);
    } else if (
      (step === "retry" || step === "reconnect") &&
      candidateRef.current
    ) {
      void connect(candidateRef.current);
    }
  }

  return (
    <QuickInput
      key={`${step}:${saving}`}
      title={title}
      description={description}
      mode={mode}
      error={error}
      onSubmit={submit}
      onCancel={close}
      onBack={onBack}
    />
  );
}
