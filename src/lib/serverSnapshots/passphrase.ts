/**
 * The rule a server snapshot passphrase must meet, as the agent checks it
 * (agent/src/snapshot/format.rs check_passphrase): at least 12 characters
 * with three kinds of characters, or at least 20 of any kind. The file holds
 * every secret of the server and can be copied anywhere; the passphrase is
 * all that protects it.
 *
 * Pure: the snapshot form checks as the operator types, the server action
 * before anything is sent.
 */
export function passphraseProblem(p: string): string | null {
  const len = [...p].length;
  if (len < 12) return "At least 12 characters.";
  if (len >= 20) return null;
  const kinds = [/\p{Ll}/u, /\p{Lu}/u, /[0-9]/, /[^\p{L}\p{N}]/u].filter((re) => re.test(p)).length;
  if (kinds < 3) {
    return "Under 20 characters it needs three of: lowercase, uppercase, digits, other characters.";
  }
  return null;
}

/** 30 random characters in dash-separated groups (no 0/O/1/l look-alikes):
 *  easy to copy, far past the 20-character rule. */
export function generatePassphrase(): string {
  const alphabet = "abcdefghijkmnpqrstuvwxyz23456789";
  const bytes = new Uint8Array(30);
  crypto.getRandomValues(bytes);
  const chars = [...bytes].map((b) => alphabet[b % alphabet.length]);
  const groups: string[] = [];
  for (let i = 0; i < chars.length; i += 5) groups.push(chars.slice(i, i + 5).join(""));
  return groups.join("-");
}
