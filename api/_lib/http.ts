type JsonValue = Record<string, unknown> | unknown[];

declare const process: {
  env: Record<string, string | undefined>;
};

export function json(status: number, body: JsonValue): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

export async function readJson(request: Request): Promise<unknown> {
  const text = await request.text();
  if (!text.trim()) {
    return {};
  }

  try {
    return JSON.parse(text);
  } catch {
    throw new HttpError(400, "invalid_json", "Request body must be valid JSON.");
  }
}

export function requireEnv(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) {
    throw new HttpError(
      500,
      "missing_server_config",
      `Missing required server environment variable: ${name}.`,
    );
  }

  return value;
}

export function optionalEnv(name: string): string | undefined {
  const value = process.env[name]?.trim();
  return value || undefined;
}

export function bearerToken(request: Request): string | undefined {
  const authorization = request.headers.get("authorization")?.trim();
  if (!authorization) {
    return undefined;
  }

  const [scheme, token] = authorization.split(/\s+/, 2);
  if (scheme?.toLowerCase() !== "bearer" || !token) {
    return undefined;
  }

  return token;
}

export async function proxyJson(
  url: string,
  init: RequestInit,
): Promise<Response> {
  const response = await fetch(url, init);
  const headers = copyHeaders(response.headers);
  headers.set("cache-control", "no-store");

  return new Response(response.body, {
    status: response.status,
    headers,
  });
}

export function copyHeaders(source: Headers): Headers {
  const headers = new Headers();
  for (const [key, value] of source.entries()) {
    if (key.toLowerCase() === "content-encoding") {
      continue;
    }
    headers.set(key, value);
  }
  return headers;
}

export function upstreamUrl(base: string, path: string): string {
  return `${base.replace(/\/+$/, "")}/${path.replace(/^\/+/, "")}`;
}

export class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

export function errorResponse(error: unknown): Response {
  if (error instanceof HttpError) {
    return json(error.status, {
      error: error.code,
      error_description: error.message,
    });
  }

  const message = error instanceof Error ? error.message : "Unexpected server error.";
  return json(500, {
    error: "server_error",
    error_description: message,
  });
}
