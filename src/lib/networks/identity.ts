import "server-only";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { config } from "../config";
import { getSetting } from "../settings";
import { b64url, fingerprint, normalizePeerUrl } from "./protocol";

/**
 * This server's identity in My Networks: an Ed25519 key made on first use
 * and kept next to panel.db (node-identity.key, 0600, only the panel user
 * reads it). Other servers pin the public half when this one joins; a new
 * key means leaving and joining every network again.
 */
export type Identity = {
  publicKey: string;
  fingerprint: string;
  sign: (message: string) => string;
};

const KEY_FILE = "node-identity.key";

let cached: Identity | null = null;

export function nodeIdentity(): Identity {
  if (cached) return cached;
  const file = path.join(path.dirname(config.panelDbPath), KEY_FILE);
  let pem: string;
  try {
    pem = fs.readFileSync(file, "utf8");
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== "ENOENT") throw e;
    const { privateKey } = crypto.generateKeyPairSync("ed25519");
    pem = privateKey.export({ type: "pkcs8", format: "pem" }).toString();
    try {
      // wx: when two requests race to create it, the first file wins.
      fs.writeFileSync(file, pem, { mode: 0o600, flag: "wx" });
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== "EEXIST") throw err;
      pem = fs.readFileSync(file, "utf8");
    }
  }
  const privateKey = crypto.createPrivateKey(pem);
  const jwk = crypto.createPublicKey(privateKey).export({ format: "jwk" });
  const publicKey = String(jwk.x);
  cached = {
    publicKey,
    fingerprint: fingerprint(publicKey),
    sign: (message) => b64url(crypto.sign(null, Buffer.from(message), privateKey)),
  };
  return cached;
}

/** How this server shows up in networks: set on the page, else its host name. */
export function ownName(): string {
  return getSetting("networks.node_name", "") || config.panelHost || os.hostname();
}

/**
 * The address other servers use for this one: https:// and the panel's host
 * name. NETWORK_PUBLIC_URL in /etc/vps-panel/.env overrides it, for a panel
 * that other servers reach through another name or port.
 */
export function ownUrl(): { url: string; problem: null } | { url: null; problem: string } {
  const override = process.env.NETWORK_PUBLIC_URL;
  try {
    if (override) return { url: normalizePeerUrl(override), problem: null };
    if (!config.panelHost || /^[\d.]+$/.test(config.panelHost) || config.panelHost.includes(":")) {
      return {
        url: null,
        problem:
          "This server has no panel hostname yet (it is reached by IP). Servers reach each other over HTTPS by name: set one under Settings > Panel hostname and let it issue the certificate.",
      };
    }
    return { url: normalizePeerUrl(`https://${config.panelHost}`), problem: null };
  } catch (e) {
    return { url: null, problem: e instanceof Error ? e.message : String(e) };
  }
}
