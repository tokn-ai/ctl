import type { useVpnEnrollment } from "../../features/vpn/useVpnEnrollment";
import { vpnNeedsSignIn } from "../../features/vpn/status";

interface Props {
  enrollment: ReturnType<typeof useVpnEnrollment>;
}

export function VpnEnrollmentProgress({ enrollment }: Props) {
  const status = enrollment.snapshot?.status;
  const needs_sign_in = vpnNeedsSignIn(status);
  const failure = enrollment.snapshot?.error?.message ?? enrollment.error;
  const message = enrollment.cancelling ? "Canceling sign-in…"
    : enrollment.starting ? "Starting Tailscale…"
      : failure ? "Could not complete sign-in"
        : enrollment.ready ? "Signed in to Tailscale"
          : enrollment.opening_browser ? "Opening your browser…"
            : needs_sign_in ? "Finish signing in with your browser"
              : status?.message ?? "Starting Tailscale…";

  return <section className="vpn-enrollment" aria-label="Tailscale sign-in">
    <p role="status" aria-live="polite"><strong>{message}</strong></p>
    {enrollment.ready ? <>
      <dl className="vpn-connection-details">
        {status?.username ? <div><dt>Account</dt><dd>{status.username}</dd></div> : null}
        {status?.tailnet ? <div><dt>Tailnet</dt><dd>{status.tailnet}</dd></div> : null}
        {status?.hostname ? <div><dt>Device name in Tailscale</dt><dd>{status.hostname}</dd></div> : null}
      </dl>
      <p>Confirm this is the account you want, then save the connection.</p>
    </> : !failure && !enrollment.cancelling ? <p>
      {needs_sign_in ? "Complete sign-in in the browser. This dialog updates automatically when you return."
        : "Preparing your connection. You can cancel while it starts."}
    </p> : null}
    {needs_sign_in && !enrollment.cancelling ? <button type="button" disabled={enrollment.opening_browser || enrollment.starting} onClick={() => void enrollment.openBrowser()}>
      {enrollment.opening_browser ? "Opening browser…" : "Open browser"}
    </button> : null}
    {enrollment.error && enrollment.snapshot && !enrollment.snapshot.error && !enrollment.cancelling ? <button type="button" onClick={() => void enrollment.refresh()}>Refresh status</button> : null}
    {failure ? <p className="vpn-error" role="alert">{failure}</p> : null}
    {enrollment.browser_error ? <p className="vpn-error" role="alert">{enrollment.browser_error} Choose Open browser to try again.</p> : null}
  </section>;
}
