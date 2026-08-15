// Shared utilities used by both api and web packages.

// Seeded defect: leftover debug logging in production code.
export function logDebug(...parts: unknown[]): void {
  console.log("[debug]", ...parts);
}

// Seeded defect: explicit any erases the payload type.
export function parseJson(raw: string): any {
  return JSON.parse(raw);
}

// Seeded defect: explicit any erases the options type.
export function mergeOptions(base: any, extra: any): any {
  return { ...base, ...extra };
}

export function slugify(input: string): string {
  return input
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-");
}

export function truncate(text: string, max: number): string {
  return text.length > max ? text.slice(0, max - 1) + "…" : text;
}
