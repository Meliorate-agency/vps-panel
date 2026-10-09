"use server";

import { revalidatePath } from "next/cache";
import { agent, type SshRootLogin } from "@/lib/agent";
import { requireAdmin } from "@/lib/auth";
import { writeAudit } from "@/lib/audit";

export type ActionResult = { ok: true } | { ok: false; error: string };

/**
 * Defense-in-depth check that `name` is a real Linux account before we ask
 * the agent to write SSH-key files for it. The agent is the actual security
 * boundary (it resolves the home dir via getpwnam and rejects unknown users),
 * but validating here gives the operator an immediate, clear error instead of
 * a raw "agent 400: no such user" — and stops a tampered form POST (the user
 * field is a dropdown, but dropdowns are trivially editable) from reaching the
 * socket with a bogus name.
 *
 * If the user list can't be fetched (agent/socket down), we fall through to
 * `true` rather than hard-blocking: the same outage would fail the subsequent
 * write anyway, and we'd rather surface that real error than a misleading
 * "no such user". This is a convenience gate, not the trust boundary.
 */
async function isKnownSystemUser(name: string): Promise<boolean> {
  if (!name) return false;
  try {
    const list = await agent.system.users();
    return list.some((u) => u.name === name);
  } catch {
    return true;
  }
}

const ROOT_LOGIN_MODES: readonly SshRootLogin[] = [
  "yes",
  "prohibit-password",
  "forced-commands-only",
  "no",
];

export async function setSshServerConfigAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const str = (k: string) => String(fd.get(k) ?? "").trim();

  // Only fields that differ from the live values (the form's current_*
  // inputs) are sent. The form used to send everything, so saving a new port
  // also rewrote the root-login mode it had displayed wrongly.
  const body: {
    port?: number;
    idle_timeout_minutes?: number;
    password_auth?: boolean;
    permit_root_login?: SshRootLogin;
  } = {};
  const portRaw = str("port");
  if (portRaw && portRaw !== str("current_port")) {
    const p = Number(portRaw);
    if (!Number.isInteger(p) || p < 1 || p > 65535) {
      return { ok: false, error: "port must be an integer between 1 and 65535" };
    }
    body.port = p;
  }
  const idleRaw = str("idle_timeout_minutes");
  if (idleRaw && idleRaw !== str("current_idle_timeout_minutes")) {
    const m = Number(idleRaw);
    if (!Number.isInteger(m) || m < 0 || m > 1440) {
      return { ok: false, error: "idle timeout must be 0..1440 minutes" };
    }
    body.idle_timeout_minutes = m;
  }
  // An unchecked checkbox is not submitted at all, so "off" is its absence.
  const passwordAuth = fd.get("password_auth") === "on";
  if (passwordAuth !== (str("current_password_auth") === "1")) {
    body.password_auth = passwordAuth;
  }
  const rootLogin = str("permit_root_login");
  if (rootLogin && rootLogin !== str("current_permit_root_login")) {
    if (!ROOT_LOGIN_MODES.includes(rootLogin as SshRootLogin)) {
      return { ok: false, error: "unknown root login mode" };
    }
    body.permit_root_login = rootLogin as SshRootLogin;
  }
  if (Object.keys(body).length === 0) {
    return { ok: false, error: "Nothing changed." };
  }

  try {
    await agent.ssh.setConfig(body);
    writeAudit({
      actorId: s.userId,
      action: "ssh.config.set",
      target: JSON.stringify(body),
      result: "ok",
    });
    revalidatePath("/settings");
    revalidatePath("/users");
    revalidatePath("/setup");
    return { ok: true };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "ssh.config.set",
      target: JSON.stringify(body),
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function addSshKeyAction(
  _prev: ActionResult | null,
  fd: FormData,
): Promise<ActionResult> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const key = String(fd.get("key") ?? "").trim();
  const label = String(fd.get("label") ?? "").trim() || undefined;
  if (!(await isKnownSystemUser(user))) {
    return { ok: false, error: `no such Linux user: ${user || "(empty)"}` };
  }
  try {
    await agent.ssh.add({ user, key, label });
    writeAudit({ actorId: s.userId, action: "ssh.keys.add", target: user, result: "ok" });
    revalidatePath("/ssh-keys");
    revalidatePath("/setup");
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function removeSshKeyAction(
  user: string,
  fp: string,
  purgeKeypair?: string,
): Promise<void> {
  const s = await requireAdmin();
  await agent.ssh.remove(fp, user, purgeKeypair);
  writeAudit({
    actorId: s.userId,
    action: "ssh.keys.remove",
    target: `${user}/${fp}`,
    result: "ok",
    message: purgeKeypair
      ? `purged keypair files ~/.ssh/${purgeKeypair} (+ .pub, config Host block)`
      : undefined,
  });
  revalidatePath("/ssh-keys");
}

export async function fetchSshPublicKeyAction(
  user: string,
  fp: string,
): Promise<{ ok: true; content: string } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    const resp = await agent.ssh.getPublicKey(fp, user);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.view_public",
      target: `${user}/${fp}`,
      result: "ok",
    });
    return { ok: true, content: resp.content };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

// Private-key reads are audit-logged as a sensitive action — the operator
// is exporting key material via the panel, which is the closest thing the
// system has to a "credential reveal" event.
export async function fetchSshPrivateKeyAction(
  user: string,
  name: string,
): Promise<{ ok: true; content: string } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    const resp = await agent.ssh.getPrivateKey(user, name);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.view_private",
      target: `${user}/${name}`,
      result: "ok",
    });
    return { ok: true, content: resp.content };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.view_private",
      target: `${user}/${name}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function updateSshPublicKeyAction(
  user: string,
  fp: string,
  content: string,
): Promise<{ ok: true; fingerprint: string | null } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    const resp = await agent.ssh.updatePublicKey(fp, user, content);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.update_public",
      target: `${user}/${fp}`,
      result: "ok",
      message: resp.fingerprint ? `new_fp=${resp.fingerprint}` : undefined,
    });
    revalidatePath("/ssh-keys");
    return { ok: true, fingerprint: resp.fingerprint };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.update_public",
      target: `${user}/${fp}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

type FileFetchResult =
  | { ok: true; content: string }
  | { ok: false; error: string };

type FileSaveResult = { ok: true } | { ok: false; error: string };

async function fetchSshFile(
  user: string,
  which: "authorized_keys" | "config",
): Promise<FileFetchResult> {
  await requireAdmin();
  try {
    const resp =
      which === "authorized_keys"
        ? await agent.ssh.getAuthorizedKeysFile(user)
        : await agent.ssh.getConfigFile(user);
    return { ok: true, content: resp.content };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

async function saveSshFile(
  user: string,
  which: "authorized_keys" | "config",
  content: string,
): Promise<FileSaveResult> {
  const s = await requireAdmin();
  try {
    if (which === "authorized_keys") {
      await agent.ssh.putAuthorizedKeysFile(user, content);
    } else {
      await agent.ssh.putConfigFile(user, content);
    }
    writeAudit({
      actorId: s.userId,
      action: `ssh.files.${which}.write`,
      target: user,
      result: "ok",
      message: `${content.length} bytes`,
    });
    return { ok: true };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: `ssh.files.${which}.write`,
      target: user,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

export async function fetchAuthorizedKeysFileAction(user: string) {
  return fetchSshFile(user, "authorized_keys");
}
export async function saveAuthorizedKeysFileAction(user: string, content: string) {
  return saveSshFile(user, "authorized_keys", content);
}
export async function fetchSshConfigFileAction(user: string) {
  return fetchSshFile(user, "config");
}
export async function saveSshConfigFileAction(user: string, content: string) {
  return saveSshFile(user, "config", content);
}

export async function updateSshPrivateKeyAction(
  user: string,
  name: string,
  content: string,
): Promise<{ ok: true } | { ok: false; error: string }> {
  const s = await requireAdmin();
  try {
    await agent.ssh.updatePrivateKey(user, name, content);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.update_private",
      target: `${user}/${name}`,
      result: "ok",
    });
    revalidatePath("/ssh-keys");
    return { ok: true };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keys.update_private",
      target: `${user}/${name}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}

const KEYPAIR_NAME_RE = /^[a-z0-9][a-z0-9_-]{0,30}[a-z0-9]$/;

export type GenerateKeypairResult =
  | { ok: true; publicKey: string; privateKeyPath: string; publicKeyPath: string }
  | { ok: false; error: string };

// Generate an ed25519 keypair on the server, append the .pub to
// authorized_keys (so the same keypair also works for inbound SSH), and
// register a Host alias in ~/.ssh/config pointing at github.com. Typical
// use case: lets the panel itself git-clone from a private GitHub repo
// without the operator hand-rolling ssh-keygen.
export async function generateSshKeypairAction(
  _prev: GenerateKeypairResult | null,
  fd: FormData,
): Promise<GenerateKeypairResult> {
  const s = await requireAdmin();
  const user = String(fd.get("user") ?? "").trim();
  const name = String(fd.get("name") ?? "").trim().toLowerCase();

  if (!KEYPAIR_NAME_RE.test(name)) {
    return {
      ok: false,
      error:
        "Keypair name must be lowercase letters / digits / `_` / `-`, 2-32 chars, no spaces.",
    };
  }
  if (!(await isKnownSystemUser(user))) {
    return { ok: false, error: `no such Linux user: ${user || "(empty)"}` };
  }

  try {
    const resp = await agent.ssh.generateKeypair({ user, name });
    writeAudit({
      actorId: s.userId,
      action: "ssh.keypair.generate",
      target: `${user}/${name}`,
      result: "ok",
    });
    revalidatePath("/ssh-keys");
    return {
      ok: true,
      publicKey: resp.public_key,
      privateKeyPath: resp.private_key_path,
      publicKeyPath: resp.public_key_path,
    };
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    writeAudit({
      actorId: s.userId,
      action: "ssh.keypair.generate",
      target: `${user}/${name}`,
      result: "error",
      message: msg,
    });
    return { ok: false, error: msg };
  }
}
