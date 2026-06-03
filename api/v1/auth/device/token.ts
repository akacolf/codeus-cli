import {
  copyHeaders,
  errorResponse,
  json,
  optionalEnv,
  readJson,
  requireEnv,
  upstreamUrl,
} from "../../../_lib/http";

export default {
  async fetch(request: Request): Promise<Response> {
    if (request.method !== "POST") {
      return new Response(null, { status: 405 });
    }

    try {
      const authUrl =
        optionalEnv("CODEUS_DEVICE_TOKEN_URL") ??
        upstreamUrl(requireEnv("CODEUS_AUTH_BASE"), "device/token");
      const body = await readJson(request);
      const payload =
        typeof body === "object" && body !== null
          ? (body as Record<string, unknown>)
          : {};
      const clientId = optionalEnv("CODEUS_OAUTH_CLIENT_ID") ?? "codeus-cli";

      const response = await fetch(authUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          ...payload,
          client_id: clientId,
          deviceCode: payload.deviceCode ?? payload.device_code,
        }),
      });

      const text = await response.text();
      const upstream = text.trim()
        ? (JSON.parse(text) as Record<string, unknown>)
        : {};

      if (response.status === 202 || upstream.status === "pending") {
        return json(400, {
          error: "authorization_pending",
          error_description: "Waiting for browser approval.",
        });
      }

      if (response.status === 410 || upstream.status === "expired") {
        return json(400, {
          error: "expired_token",
          error_description: "Browser login expired.",
        });
      }

      if (upstream.status === "denied") {
        return json(400, {
          error: "access_denied",
          error_description: "Browser login was denied.",
        });
      }

      if (!response.ok) {
        return new Response(text, {
          status: response.status,
          headers: copyHeaders(response.headers),
        });
      }

      if (typeof upstream.sessionToken === "string") {
        return json(200, {
          access_token: upstream.sessionToken,
          session_token: upstream.sessionToken,
          token_type: "Bearer",
          expires_in:
            typeof upstream.expiresAt === "number"
              ? Math.max(1, Math.floor((upstream.expiresAt - Date.now()) / 1000))
              : undefined,
        });
      }

      return json(200, upstream);
    } catch (error) {
      return errorResponse(error);
    }
  },
};
