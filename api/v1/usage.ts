import {
  bearerToken,
  errorResponse,
  HttpError,
  optionalEnv,
  proxyJson,
  requireEnv,
  upstreamUrl,
} from "../_lib/http";

export default {
  async fetch(request: Request): Promise<Response> {
    if (request.method !== "GET") {
      return new Response(null, { status: 405 });
    }

    try {
      const usageUrl =
        optionalEnv("CODEUS_USAGE_URL") ??
        upstreamUrl(
          optionalEnv("CODEUS_ACCOUNT_BASE") ?? requireEnv("CODEUS_AUTH_BASE"),
          "usage",
        );
      const token = bearerToken(request);
      if (!token) {
        throw new HttpError(401, "not_authenticated", "Missing Codeus session token.");
      }

      return proxyJson(usageUrl, {
        method: "GET",
        headers: {
          authorization: `Bearer ${token}`,
        },
      });
    } catch (error) {
      return errorResponse(error);
    }
  },
};
