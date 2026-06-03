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
        optionalEnv("CODEUS_DEVICE_CODE_URL") ??
        upstreamUrl(requireEnv("CODEUS_AUTH_BASE"), "device/code");
      const body = await readJson(request);
      const clientId = optionalEnv("CODEUS_OAUTH_CLIENT_ID") ?? "codeus-cli";
      const scope = optionalEnv("CODEUS_OAUTH_SCOPE") ?? "chat account usage";

      const response = await fetch(authUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          ...(typeof body === "object" && body !== null ? body : {}),
          client_id: clientId,
          scope,
        }),
      });

      const text = await response.text();
      if (!response.ok) {
        return new Response(text, {
          status: response.status,
          headers: copyHeaders(response.headers),
        });
      }

      const upstream = JSON.parse(text) as Record<string, unknown>;
      if (typeof upstream.deviceCode === "string") {
        return json(200, {
          device_code: upstream.deviceCode,
          user_code: upstream.userCode,
          verification_uri: upstream.linkUrl,
          verification_uri_complete: upstream.linkUrl,
          expires_in:
            typeof upstream.expiresAt === "number"
              ? Math.max(1, Math.floor((upstream.expiresAt - Date.now()) / 1000))
              : 600,
          interval: upstream.interval,
        });
      }

      return json(200, upstream);
    } catch (error) {
      return errorResponse(error);
    }
  },
};
