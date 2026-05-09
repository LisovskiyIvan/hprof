const responseCache = new Map<string, Promise<unknown>>();

interface FetchJsonOptions {
  cache?: boolean;
}

export async function fetchJson<T>(url: string, options?: FetchJsonOptions): Promise<T> {
  const useCache = options?.cache ?? true;
  if (useCache) {
    const cached = responseCache.get(url);
    if (cached) return cached as Promise<T>;
  }

  const request = fetch(url)
    .then(async (response) => {
      const json = await response.json();
      if (!response.ok) {
        throw new Error(typeof json?.error === "string" ? json.error : `HTTP ${response.status}`);
      }
      return json as T;
    })
    .catch((error) => {
      responseCache.delete(url);
      throw error;
    });

  if (useCache) {
    responseCache.set(url, request);
  }

  return request;
}
