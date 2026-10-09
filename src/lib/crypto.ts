import "server-only";
import { hash as argonHash, verify as argonVerify } from "@node-rs/argon2";

const PARAMS = {
  memoryCost: 64 * 1024,
  timeCost: 3,
  parallelism: 4,
};

export async function hashPassword(password: string): Promise<string> {
  return argonHash(password, PARAMS);
}

let dummyHash: Promise<string> | null = null;

/** A real argon2id hash of a random secret, for spending the same time on a
 *  login attempt for an unknown email as for a wrong password. */
export function dummyPasswordHash(): Promise<string> {
  dummyHash ??= argonHash(crypto.randomUUID(), PARAMS);
  return dummyHash;
}

export async function verifyPassword(hash: string, password: string): Promise<boolean> {
  try {
    return await argonVerify(hash, password);
  } catch {
    return false;
  }
}
