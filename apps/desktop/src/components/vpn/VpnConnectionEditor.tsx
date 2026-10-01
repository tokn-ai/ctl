import { useEffect, useRef, useState, type FormEvent } from "react";
import type { VpnConnection, VpnConnectionInput, VpnProvider } from "../../lib/types";
import { useVpnEnrollment } from "../../features/vpn/useVpnEnrollment";
import { VpnEnrollmentProgress } from "./VpnEnrollmentProgress";
import { errorMessage } from "../../lib/errors";
import { QuickInputFrame } from "../commands/QuickInputFrame";

interface Props {
  connection: VpnConnection | null;
  saving: boolean;
  error: string | null;
  on_save(connection: VpnConnectionInput): Promise<boolean>;
  on_close(): void;
  enrollment_supported?: boolean;
  on_enrollment_change?(connection_id: string | null): void;
  on_save_enrollment?(enrollment_id: string): Promise<boolean>;
}

/** Passwords remain in the masked input, never in a recoverable React draft. */
export function VpnConnectionEditor({ connection, saving, error, on_save, on_close, enrollment_supported = false, on_enrollment_change, on_save_enrollment }: Props) {
  const [connection_id] = useState(() => connection?.connection_id ?? crypto.randomUUID());
  const openconnect = connection?.provider === "tailscale" ? null : connection;
  const tailscale = connection?.provider === "tailscale" ? connection : null;
  const [provider, setProvider] = useState<VpnProvider>(connection?.provider ?? "openconnect");
  const [hostname, setHostname] = useState(tailscale?.hostname ?? "");
  const [accept_routes, setAcceptRoutes] = useState(tailscale?.accept_routes ?? false);
  const [name, setName] = useState(connection?.name ?? "");
  const [url, setUrl] = useState(openconnect?.url ?? "");
  const [username, setUsername] = useState(openconnect?.username ?? "");
  const [auth_method, setAuthMethod] = useState(openconnect?.auth_method ?? "");
  const [target_ip, setTargetIp] = useState(openconnect?.target_ip ?? "");
  const [advanced, setAdvanced] = useState(Boolean(openconnect?.auth_method || openconnect?.target_ip));
  const [validation_error, setValidationError] = useState<string | null>(null);
  const [password_retry, setPasswordRetry] = useState(false);
  const password_input = useRef<HTMLInputElement>(null);
  const submitting = useRef(false);
  const enrollment = useVpnEnrollment({ on_connection_id: on_enrollment_change, on_save: on_save_enrollment, on_close });
  const enrolling = !connection && provider === "tailscale";
  const locked = saving || (enrolling && enrollment.started);
  const enrollment_failed = Boolean(enrollment.snapshot?.error || (enrollment.error && !enrollment.snapshot));

  useEffect(() => {
    const input = password_input.current;
    return () => { if (input) input.value = ""; };
  }, []);

  function clearPassword() {
    if (password_input.current) password_input.current.value = "";
  }

  function close() {
    if (saving || (!enrolling && submitting.current)) return;
    clearPassword();
    if (enrolling && enrollment.started) void enrollment.cancel();
    else on_close();
  }

  async function save(event: FormEvent) {
    event.preventDefault();
    if (saving || submitting.current) return;
    const password = password_input.current?.value ?? "";
    const input: VpnConnectionInput = provider === "tailscale" ? {
      provider, connection_id, name: name.trim(), hostname: hostname.trim() || null, accept_routes,
    } : {
      provider,
      connection_id,
      name: name.trim(),
      url: url.trim(),
      username: username.trim(),
      password: password || null,
      auth_method: auth_method.trim() || null,
      target_ip: target_ip.trim() || null,
    };
    const invalid = validateVpnInput(input, Boolean(openconnect?.has_password) && !password_retry);
    if (invalid) {
      setValidationError(invalid);
      return;
    }
    setValidationError(null);
    if (enrolling) {
      if (!enrollment_supported || input.provider !== "tailscale") return;
      if (!enrollment.started || enrollment_failed) {
        await enrollment.begin({ name: input.name, hostname: input.hostname, accept_routes: input.accept_routes });
      } else if (enrollment.ready) {
        await enrollment.save();
      }
      return;
    }
    submitting.current = true;
    clearPassword();
    try {
      const saved = await on_save(input);
      if (!saved && password) setPasswordRetry(true);
    } catch (failure) {
      setValidationError(errorMessage(failure));
      if (password) setPasswordRetry(true);
    } finally {
      submitting.current = false;
    }
  }

  return (
    <QuickInputFrame
      title={connection ? "Edit VPN connection" : "Add VPN connection"}
      className="vpn-editor"
      onDismiss={close}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          close();
        }
      }}
    >
      <header className="quick-input-heading vpn-editor-heading">
        <strong>{connection ? "Edit VPN connection" : "Add VPN connection"}</strong>
        <button type="button" onClick={close} disabled={saving}>Close</button>
      </header>
      <form className="vpn-editor-form" onSubmit={(event) => void save(event)}>
        <label>
          Provider
          <select value={provider} disabled={locked || Boolean(connection)} onChange={(event) => {
            clearPassword();
            setValidationError(null);
            setProvider(event.target.value as VpnProvider);
          }}>
            <option value="openconnect">OpenConnect</option>
            <option value="tailscale">Tailscale</option>
          </select>
          {connection ? <small>Create a new connection to use a different provider.</small> : null}
        </label>
        <label>
          Name
          <input autoFocus value={name} onChange={(event) => setName(event.target.value)} disabled={locked} autoComplete="off" placeholder="Work VPN" />
        </label>
        {provider === "openconnect" ? <>
          <label>
            VPN server
            <input value={url} onChange={(event) => setUrl(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder="https://vpn.example.test" />
          </label>
          <label>
            Username
            <input value={username} onChange={(event) => setUsername(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} />
          </label>
          <label>
            Password
            <input ref={password_input} type="password" aria-label="Password" disabled={locked} autoComplete="new-password" placeholder={openconnect?.has_password ? "Leave blank to keep the saved password" : undefined} />
            {openconnect?.has_password && !password_retry ? <small>Leave blank to keep the saved password.</small> : null}
            {password_retry ? <small>Re-enter the password before retrying the save.</small> : null}
          </label>
          <button type="button" className="vpn-advanced-toggle" aria-expanded={advanced} aria-controls="vpn-advanced-fields" onClick={() => setAdvanced((current) => !current)} disabled={saving}>
            {advanced ? "Hide advanced options" : "Advanced options"}
          </button>
          {advanced ? (
            <div id="vpn-advanced-fields" className="vpn-advanced-fields">
              <label>
                Authentication method
                <input value={auth_method} aria-label="Authentication method" onChange={(event) => setAuthMethod(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder="Optional" />
                <small>Use the method name supplied by your VPN provider, if required.</small>
              </label>
              <label>
                Connectivity check target
                <input value={target_ip} aria-label="Connectivity check target" onChange={(event) => setTargetIp(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder="Optional IPv4 address" />
                <small>Checks SSH access after connecting. A failed check does not stop the VPN.</small>
              </label>
            </div>
          ) : null}
        </> : <>
          <p className="vpn-editor-note">Sign in with your Tailscale account. This connection keeps its login across restarts.</p>
          <button type="button" className="vpn-advanced-toggle" aria-expanded={advanced} aria-controls="tailscale-advanced-fields" onClick={() => setAdvanced((current) => !current)} disabled={saving}>
            {advanced ? "Hide advanced options" : "Advanced options"}
          </button>
          {advanced ? <div id="tailscale-advanced-fields" className="vpn-advanced-fields">
            <label>
              Device name in Tailscale
              <input value={hostname} aria-label="Device name in Tailscale" onChange={(event) => setHostname(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder="Assigned automatically" />
              <small>The name shown in Tailscale’s device list. Leave blank for ctmux to choose one.</small>
            </label>
            <label className="vpn-checkbox-option">
              <input type="checkbox" checked={accept_routes} onChange={(event) => setAcceptRoutes(event.target.checked)} disabled={locked} />
              <span>Accept subnet routes</span>
              <small>Reach networks advertised by other devices in your tailnet.</small>
            </label>
          </div> : null}
          {enrolling && enrollment.started ? <VpnEnrollmentProgress enrollment={enrollment} /> : null}
          {enrolling && !enrollment_supported ? <p className="vpn-error" role="alert">Update and restart ctld to sign in before saving a Tailscale connection.</p> : null}
        </>}
        <p className="vpn-editor-note">{enrolling ? "Your connection appears in the VPN list after you sign in and choose Save connection." : "Settings are saved privately on this Mac."}</p>
        {validation_error || error ? <p className="vpn-error" role="alert">{validation_error ?? error}</p> : null}
        <footer className="vpn-editor-actions">
          <button type="button" onClick={close} disabled={saving || enrollment.cancelling}>{enrollment.cancelling ? "Canceling…" : "Cancel"}</button>
          <button type="submit" className="button-primary" disabled={saving || (enrolling && (!enrollment_supported || enrollment.starting || enrollment.cancelling || (enrollment.started && !enrollment.ready && !enrollment_failed)))}>
            {saving ? "Saving…" : !enrolling || enrollment.ready ? "Save connection" : enrollment_failed ? "Try again" : enrollment.started ? "Waiting for sign-in…" : "Sign in with Tailscale"}
          </button>
        </footer>
      </form>
    </QuickInputFrame>
  );
}

export function validateVpnInput(input: VpnConnectionInput, has_password: boolean): string | null {
  if (!input.name) return "Enter a connection name.";
  if (input.provider === "tailscale") {
    if (/[\x00-\x1f\x7f]/u.test(input.name)) return "Use single-line values without control characters.";
    if (input.hostname !== null && !/^(?=.{1,63}$)[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?$/u.test(input.hostname)) {
      return "Use a device name with letters, numbers, and hyphens (up to 63 characters).";
    }
    return null;
  }
  if (!input.url) return "Enter the VPN server.";
  if (!input.username) return "Enter your username.";
  if (!input.password && !has_password) return "Enter your password.";
  if ([input.name, input.url, input.username, input.password, input.auth_method, input.target_ip].some((value) => value !== null && /[\r\n\0]/u.test(value))) {
    return "Use single-line values without control characters.";
  }
  if (input.url.includes("://") && !input.url.startsWith("https://")) return "Use an HTTPS VPN server address.";
  if (input.target_ip !== null && !validIpv4(input.target_ip)) return "Enter a valid IPv4 address for the optional connectivity check.";
  return null;
}

function validIpv4(value: string): boolean {
  const octets = value.split(".");
  return octets.length === 4 && octets.every((octet) => /^\d{1,3}$/u.test(octet) && Number(octet) <= 255);
}
