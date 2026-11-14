import { formatText as formatMessageText, message, type DisplayText, type Locale } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import type { VpnConnection, VpnConnectionInput, VpnProvider } from "../../lib/types";
import { useVpnEnrollment } from "../../features/vpn/useVpnEnrollment";
import { VpnEnrollmentProgress } from "./VpnEnrollmentProgress";
import { localizedErrorText } from "../../lib/errors";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface Props {
  connection: VpnConnection | null;
  saving: boolean;
  error: DisplayText | null;
  on_save(connection: VpnConnectionInput): Promise<boolean>;
  on_close(): void;
  enrollment_supported?: boolean;
  on_enrollment_change?(connection_id: string | null): void;
  on_save_enrollment?(enrollment_id: string): Promise<boolean>;
}

type ValidationField = "name" | "url" | "username" | "password" | "hostname" | "auth_method" | "target_ip" | "form";
interface VpnValidation { field: ValidationField; message: DisplayText }

/** Passwords remain in the masked input, never in a recoverable React draft. */
export function VpnConnectionEditor({ connection, saving, error, on_save, on_close, enrollment_supported = false, on_enrollment_change, on_save_enrollment }: Props) {
  const { t, formatText } = useLocale();
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
  const [validation_error, setValidationError] = useState<VpnValidation | null>(null);
  const validation_id = useId();
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
    const invalid = validateVpnFields(input, Boolean(openconnect?.has_password) && !password_retry);
    if (invalid) {
      setValidationError(invalid);
      if (["hostname", "auth_method", "target_ip"].includes(invalid.field)) setAdvanced(true);
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
      setValidationError({ field: "form", message: localizedErrorText(failure) });
      if (password) setPasswordRetry(true);
    } finally {
      submitting.current = false;
    }
  }

  function fieldError(field: ValidationField) {
    return validation_error?.field === field ? <small className="vpn-field-error" id={`${validation_id}-${field}`} role="alert">{formatText(validation_error.message)}</small> : null;
  }

  function fieldAttributes(field: ValidationField) {
    return validation_error?.field === field ? { "aria-invalid": true as const, "aria-describedby": `${validation_id}-${field}` } : {};
  }

  const form_error = validation_error?.field === "form" ? validation_error.message : validation_error ? null : error;

  return (
    <QuickInputFrame
      title={connection ? t("vpn.edit_connection") : t("vpn.add_connection")}
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
        <strong>{connection ? t("vpn.edit_connection") : t("vpn.add_connection")}</strong>
        <button type="button" onClick={close} disabled={saving}>{t("vpn.close")}</button>
      </header>
      <form className="vpn-editor-form" onSubmit={(event) => void save(event)}>
        <label>{t("vpn.provider")}
          <select value={provider} disabled={locked || Boolean(connection)} onChange={(event) => {
            clearPassword();
            setValidationError(null);
            setProvider(event.target.value as VpnProvider);
          }}>
            <option value="openconnect">OpenConnect</option>
            <option value="tailscale">Tailscale</option>
          </select>
          {connection ? <small>{t("vpn.different_provider")}</small> : null}
        </label>
        <label>{t("vpn.name")}
          <input autoFocus aria-label={t("vpn.name")} value={name} onChange={(event) => setName(event.target.value)} disabled={locked} autoComplete="off" placeholder={t("vpn.work_vpn")} {...fieldAttributes("name")} />
          {fieldError("name")}
        </label>
        {provider === "openconnect" ? <>
          <label>{t("vpn.server")}
            <input aria-label={t("vpn.server")} value={url} onChange={(event) => setUrl(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder="https://vpn.example.test" {...fieldAttributes("url")} />
            {fieldError("url")}
          </label>
          <label>{t("vpn.username")}
            <input aria-label={t("vpn.username")} value={username} onChange={(event) => setUsername(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} {...fieldAttributes("username")} />
            {fieldError("username")}
          </label>
          <label>{t("vpn.password")}
            <input ref={password_input} type="password" aria-label={t("vpn.password")} disabled={locked} autoComplete="new-password" placeholder={openconnect?.has_password ? t("vpn.keep_password_placeholder") : undefined} {...fieldAttributes("password")} />
            {fieldError("password")}
            {openconnect?.has_password && !password_retry ? <small>{t("vpn.keep_password")}</small> : null}
            {password_retry ? <small>{t("vpn.reenter_password")}</small> : null}
          </label>
          <button type="button" className="vpn-advanced-toggle" aria-expanded={advanced} aria-controls="vpn-advanced-fields" onClick={() => setAdvanced((current) => !current)} disabled={saving}>
            {advanced ? t("vpn.hide_advanced") : t("vpn.advanced")}
          </button>
          {advanced ? (
            <div id="vpn-advanced-fields" className="vpn-advanced-fields">
              <label>{t("vpn.auth_method")}
                <input value={auth_method} aria-label={t("vpn.auth_method")} onChange={(event) => setAuthMethod(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder={t("vpn.optional")} {...fieldAttributes("auth_method")} />
                {fieldError("auth_method")}
                <small>{t("vpn.auth_method_hint")}</small>
              </label>
              <label>{t("vpn.check_target")}
                <input value={target_ip} aria-label={t("vpn.check_target")} onChange={(event) => setTargetIp(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder={t("vpn.optional_ipv4")} {...fieldAttributes("target_ip")} />
                {fieldError("target_ip")}
                <small>{t("vpn.check_target_hint")}</small>
              </label>
            </div>
          ) : null}
        </> : <>
          <p className="vpn-editor-note">{t("vpn.tailscale_login_hint")}</p>
          <button type="button" className="vpn-advanced-toggle" aria-expanded={advanced} aria-controls="tailscale-advanced-fields" onClick={() => setAdvanced((current) => !current)} disabled={saving}>
            {advanced ? t("vpn.hide_advanced") : t("vpn.advanced")}
          </button>
          {advanced ? <div id="tailscale-advanced-fields" className="vpn-advanced-fields">
            <label>{t("vpn.tailscale_device_name")}
              <input value={hostname} aria-label={t("vpn.tailscale_device_name")} onChange={(event) => setHostname(event.target.value)} disabled={locked} autoComplete="off" spellCheck={false} placeholder={t("vpn.automatic_name")} {...fieldAttributes("hostname")} />
              {fieldError("hostname")}
              <small>{t("vpn.tailscale_device_hint")}</small>
            </label>
            <label className="vpn-checkbox-option">
              <input type="checkbox" checked={accept_routes} onChange={(event) => setAcceptRoutes(event.target.checked)} disabled={locked} />
              <span>{t("vpn.accept_routes")}</span>
              <small>{t("vpn.accept_routes_hint")}</small>
            </label>
          </div> : null}
          {enrolling && enrollment.started ? <VpnEnrollmentProgress enrollment={enrollment} /> : null}
          {enrolling && !enrollment_supported ? <InlineFeedback>{t("vpn.enrollment_requires_update")}</InlineFeedback> : null}
        </>}
        <p className="vpn-editor-note">{enrolling ? t("vpn.save_enrollment_hint") : t("vpn.settings_privacy")}</p>
        {form_error ? <InlineFeedback>{formatText(form_error)}</InlineFeedback> : null}
        <footer className="vpn-editor-actions">
          <button type="button" onClick={close} disabled={saving || enrollment.cancelling}>{enrollment.cancelling ? t("vpn.canceling") : t("vpn.cancel")}</button>
          <button type="submit" className="button-primary" disabled={saving || (enrolling && (!enrollment_supported || enrollment.starting || enrollment.cancelling || (enrollment.started && !enrollment.ready && !enrollment_failed)))}>
            {saving ? t("vpn.saving") : !enrolling || enrollment.ready ? t("vpn.save") : enrollment_failed ? t("vpn.retry") : enrollment.started ? t("vpn.waiting_sign_in") : t("vpn.sign_in_tailscale")}
          </button>
        </footer>
      </form>
    </QuickInputFrame>
  );
}

export function validateVpnInput(input: VpnConnectionInput, has_password: boolean, locale: Locale = "en"): string | null {
  const feedback = validateVpnFields(input, has_password)?.message;
  return feedback ? formatMessageText(locale, feedback) : null;
}

function validateVpnFields(input: VpnConnectionInput, has_password: boolean): VpnValidation | null {
  if (!input.name) return { field: "name", message: message("vpn.validation_name") };
  if (input.provider === "tailscale") {
    if (/[\x00-\x1f\x7f]/u.test(input.name)) return { field: "name", message: message("vpn.validation_single_line") };
    if (input.hostname !== null && !/^(?=.{1,63}$)[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?$/u.test(input.hostname)) {
      return { field: "hostname", message: message("vpn.validation_host_name") };
    }
    return null;
  }
  if (!input.url) return { field: "url", message: message("vpn.validation_server") };
  if (!input.username) return { field: "username", message: message("vpn.validation_username") };
  if (!input.password && !has_password) return { field: "password", message: message("vpn.validation_password") };
  const single_line_fields: [ValidationField, string | null][] = [
    ["name", input.name], ["url", input.url], ["username", input.username],
    ["password", input.password], ["auth_method", input.auth_method], ["target_ip", input.target_ip],
  ];
  const invalid_line = single_line_fields.find(([, value]) => value !== null && /[\r\n\0]/u.test(value));
  if (invalid_line) {
    return { field: invalid_line[0], message: message("vpn.validation_single_line") };
  }
  if (input.url.includes("://") && !input.url.startsWith("https://")) return { field: "url", message: message("vpn.validation_https") };
  if (input.target_ip !== null && !validIpv4(input.target_ip)) return { field: "target_ip", message: message("vpn.validation_ipv4") };
  return null;
}

function validIpv4(value: string): boolean {
  const octets = value.split(".");
  return octets.length === 4 && octets.every((octet) => /^\d{1,3}$/u.test(octet) && Number(octet) <= 255);
}
