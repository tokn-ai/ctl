import { useLocale } from "../../features/i18n/LocaleContext";
import type { useVpnEnrollment } from "../../features/vpn/useVpnEnrollment";
import { vpnNeedsSignIn } from "../../features/vpn/status";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface Props {
  enrollment: ReturnType<typeof useVpnEnrollment>;
}

export function VpnEnrollmentProgress({ enrollment }: Props) {
  const { t, formatText } = useLocale();
  const status = enrollment.snapshot?.status;
  const needs_sign_in = vpnNeedsSignIn(status);
  const failure = enrollment.snapshot?.error?.message ?? enrollment.error;
  const message = enrollment.cancelling ? t("vpn.canceling_sign_in")
    : enrollment.cleanup_pending ? t("vpn.cleanup_pending")
      : enrollment.starting ? t("vpn.starting_tailscale")
        : status?.status_unavailable ? t("vpn.tailscale_unavailable")
          : failure ? t("vpn.sign_in_failed")
            : enrollment.ready ? t("vpn.signed_in_tailscale")
              : enrollment.opening_browser ? t("vpn.opening_your_browser")
                : needs_sign_in ? t("vpn.finish_sign_in")
                  : status?.message ?? t("vpn.starting_tailscale");

  return <section className="vpn-enrollment" aria-label={t("vpn.tailscale_sign_in")}>
    <p role="status" aria-live="polite"><strong>{message}</strong></p>
    {enrollment.ready ? <>
      <dl className="vpn-connection-details">
        {status?.username ? <div><dt>{t("vpn.account")}</dt><dd>{status.username}</dd></div> : null}
        {status?.tailnet ? <div><dt>{t("vpn.tailnet")}</dt><dd>{status.tailnet}</dd></div> : null}
        {status?.hostname ? <div><dt>{t("vpn.tailscale_device_name")}</dt><dd>{status.hostname}</dd></div> : null}
      </dl>
      <p>{t("vpn.confirm_account")}</p>
    </> : !failure && !enrollment.cancelling ? <p>
      {needs_sign_in ? t("vpn.complete_sign_in")
        : t("vpn.preparing")}
    </p> : null}
    {needs_sign_in && !enrollment.cancelling && !enrollment.cleanup_pending ? <button type="button" disabled={enrollment.opening_browser || enrollment.starting} onClick={() => void enrollment.openBrowser()}>
      {enrollment.opening_browser ? t("vpn.opening_browser") : t("vpn.open_browser")}
    </button> : null}
    {enrollment.cleanup_pending && !enrollment.cancelling ? <button type="button" onClick={() => void enrollment.cancel()}>{t("vpn.retry_cleanup")}</button>
      : enrollment.error && enrollment.snapshot && !enrollment.snapshot.error && !enrollment.cancelling ? <button type="button" onClick={() => void enrollment.refresh()}>{t("vpn.refresh_status")}</button> : null}
    {failure ? <InlineFeedback>{formatText(failure)}</InlineFeedback> : null}
    {enrollment.browser_error ? <InlineFeedback>{t("vpn.browser_error", { error: enrollment.browser_error })}</InlineFeedback> : null}
  </section>;
}
