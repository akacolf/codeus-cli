import {
  errorResponse,
  optionalEnv,
  proxyJson,
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
      const clientId = optionalEnv("CODEUS_OAUTH_CLIENT_ID") ?? "codeus-cli";

      return proxyJson(authUrl, {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          ...(typeof body === "object" && body !== null ? body : {}),
          client_id: clientId,
        }),
      });
    } catch (error) {
      return errorResponse(error);
    }
  },
};
