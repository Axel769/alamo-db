// What a channel negotiated and what a certificate says (§12.2), in the
// dialog (after "Test connection", under the root CA field) and on the
// connection page. A datum the engine cannot give reads "not available":
// said, not hidden.
import { locale, t } from "./i18n";
import { CertOut, ConnSecurity } from "./ipc";

const DAY = 86_400;

function day(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString(locale(), {
    day: "numeric",
    month: "long",
    year: "numeric",
  });
}

/// The validity window as the user needs it: expired, not yet valid, about
/// to expire (30 days), or fine.
export function validity(cert: CertOut): { text: string; warn: boolean } {
  const now = Date.now() / 1000;
  if (cert.not_after === null) return { text: t("not available"), warn: false };
  if (cert.not_after < now) return { text: t("expired on {date}", { date: day(cert.not_after) }), warn: true };
  if (cert.not_before !== null && cert.not_before > now)
    return { text: t("not valid before {date}", { date: day(cert.not_before) }), warn: true };
  const days = Math.floor((cert.not_after - now) / DAY);
  if (days <= 30)
    return { text: t("expires on {date} ({days} days left)", { date: day(cert.not_after), days }), warn: true };
  return { text: t("valid until {date}", { date: day(cert.not_after) }), warn: false };
}

function certRows(cert: CertOut) {
  const na = t("not available");
  const v = validity(cert);
  return (
    <>
      <dt>{t("Subject")}</dt>
      <dd className="mono">{cert.subject ?? na}</dd>
      <dt>{t("Issuer")}</dt>
      <dd className="mono">{cert.issuer ?? na}</dd>
      <dt>{t("Validity")}</dt>
      <dd className={v.warn ? "warn" : undefined}>{v.text}</dd>
    </>
  );
}

/// A certificate on its own: the profile's root CA.
export function CertFacts({ cert }: { cert: CertOut }) {
  return <dl className="profile-summary cert-facts">{certRows(cert)}</dl>;
}

/// The facts of an open (or just tested) channel. `ssh` = the profile goes
/// through a bastion, which changes what "plaintext" means.
export function TlsFacts({ security, ssh }: { security: ConnSecurity; ssh: boolean }) {
  if (!security.encrypted) {
    return (
      <div className="banner warn tls-banner">
        {ssh
          ? t("Encrypted only up to the SSH bastion: from there to the database the traffic, password included, travels in plaintext.")
          : t("Plaintext traffic, password included.")}
      </div>
    );
  }
  const na = t("not available");
  return (
    <dl className="profile-summary tls-facts">
      <dt>{t("Protocol")}</dt>
      <dd className="mono">{security.protocol ?? na}</dd>
      <dt>{t("Cipher")}</dt>
      <dd className="mono">{security.cipher ?? na}</dd>
      <dt>{t("Server identity")}</dt>
      <dd className={security.verified ? "ok" : "warn"}>
        {security.verified
          ? t("certificate verified")
          : t("not verified: whoever controls the network can impersonate the server")}
      </dd>
      {security.cert ? (
        certRows(security.cert)
      ) : (
        <>
          <dt>{t("Certificate")}</dt>
          <dd>{na}</dd>
        </>
      )}
    </dl>
  );
}

/// One line for the connection page header; `null` in plaintext, where the
/// indicator next to the name already says so.
export function tlsSummary(security: ConnSecurity): string | null {
  if (!security.encrypted) return null;
  const parts = [
    security.protocol ?? t("TLS version not available"),
    security.cipher ?? t("cipher not available"),
    security.verified ? t("certificate verified") : t("server not verified"),
  ];
  if (security.cert) {
    if (security.cert.subject) parts.push(security.cert.subject);
    parts.push(validity(security.cert).text);
  } else {
    parts.push(t("server certificate not available"));
  }
  return parts.join(" · ");
}
