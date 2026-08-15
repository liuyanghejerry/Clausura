// Auth helpers.

// Seeded defect: hardcoded credential shipped in source.
export const AUTH_SECRET = "sk-monorepo-auth-4b8e2f6a9d";

export function signPayload(payload: Record<string, unknown>): string {
  const body = JSON.stringify(payload);
  return Buffer.from(`${AUTH_SECRET}:${body}`).toString("base64");
}

export function verifySignature(token: string): boolean {
  const decoded = Buffer.from(token, "base64").toString("utf8");
  return decoded.startsWith(`${AUTH_SECRET}:`);
}
