import "server-only";
import { TOTP, Secret } from "otpauth";
import QRCode from "qrcode";

const DIGITS = 6;
const PERIOD = 30;
const ALGO = "SHA1";

export function newSecret(): string {
  return new Secret({ size: 20 }).base32;
}

export function totpUri(email: string, secret: string, issuer = "Bolt Panel"): string {
  const t = new TOTP({
    issuer,
    label: email,
    algorithm: ALGO,
    digits: DIGITS,
    period: PERIOD,
    secret: Secret.fromBase32(secret),
  });
  return t.toString();
}

export async function totpQr(uri: string): Promise<string> {
  return QRCode.toDataURL(uri, { errorCorrectionLevel: "M", margin: 1, scale: 4 });
}

/** The 30 s time step the code belongs to, or null when it is wrong. Callers
 *  that sign someone in must also refuse a step at or below the last one they
 *  accepted (see acceptTotpStep): a valid code stays valid for up to 90 s, so
 *  anyone who saw it could otherwise use it again. */
export function verifyTotp(secret: string, token: string): number | null {
  const t = new TOTP({
    algorithm: ALGO,
    digits: DIGITS,
    period: PERIOD,
    secret: Secret.fromBase32(secret),
  });
  const delta = t.validate({ token, window: 1 });
  if (delta === null) return null;
  return Math.floor(Date.now() / 1000 / PERIOD) + delta;
}
