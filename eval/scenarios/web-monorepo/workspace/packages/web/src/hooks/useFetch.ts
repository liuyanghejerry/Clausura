import { useState, useEffect } from "react";

// Seeded defect: explicit any erases the response shape.
export function useFetch<T = any>(url: string): { data: T | null; loading: boolean } {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    // Seeded defect: leftover debug logging in production code.
    console.log(`fetching ${url}`);
    fetch(url)
      .then((r) => r.json())
      // Seeded defect: explicit any erases the parsed body.
      .then((body: any) => {
        if (!cancelled) {
          setData(body as T);
          setLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [url]);

  return { data, loading };
}
