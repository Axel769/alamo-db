// The TLS modes of a profile (`sslmode`, semantics in alamo_core::tls) in
// the user's words: label, one line on what it protects, a level for the
// icon. Which modes an engine honours comes from `engine.tls_modes`
// (§2.1): this module only describes a mode, it never decides who has it.
import { t } from "./i18n";

export type TlsLevel = "plain" | "maybe" | "encrypted" | "verified" | "full";

export type TlsModeInfo = {
  id: string;
  label: string;
  /// Short form for badges and subtitles.
  short: string;
  description: string;
  level: TlsLevel;
  recommended: boolean;
};

/// From the weakest to the strongest: the order the dialog lists them in.
export const TLS_MODE_ORDER = ["disable", "prefer", "require", "verify-ca", "verify-full"] as const;

export function tlsMode(id: string): TlsModeInfo {
  switch (id) {
    case "disable":
      return {
        id,
        label: t("No encryption"),
        short: t("plaintext"),
        description: t("Password and data travel in plaintext."),
        level: "plain",
        recommended: false,
      };
    case "prefer":
      return {
        id,
        label: t("Encrypt if the server allows it"),
        short: t("if possible"),
        description: t(
          "Without TLS on the server the connection goes on in plaintext, with no warning. The server is not authenticated.",
        ),
        level: "maybe",
        recommended: false,
      };
    case "require":
      return {
        id,
        label: t("Encryption required"),
        short: t("encrypted"),
        description: t(
          "Encrypted channel, but the server is not authenticated: whoever controls the network can impersonate it.",
        ),
        level: "encrypted",
        recommended: false,
      };
    case "verify-ca":
      return {
        id,
        label: t("Verify the certificate"),
        short: t("verified (CA)"),
        description: t(
          "The certificate must be signed by the given root CA; the host name is not checked.",
        ),
        level: "verified",
        recommended: false,
      };
    case "verify-full":
      return {
        id,
        label: t("Verify certificate and host name"),
        short: t("verified"),
        description: t("As browsers do: root CA and host name must match."),
        level: "full",
        recommended: true,
      };
    default:
      // A value this build does not know (hand-edited profile): shown as
      // is, never dressed up as something it is not.
      return { id, label: id, short: id, description: "", level: "plain", recommended: false };
  }
}

/// verify-ca / verify-full: the server certificate chain is verified.
export function verifiesChain(id: string): boolean {
  return id === "verify-ca" || id === "verify-full";
}
